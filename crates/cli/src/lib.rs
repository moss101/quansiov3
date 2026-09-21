//! quansio CLI over the public API v1 (APP-013).
//!
//! Canonical owner (DOSSIER.md §17): `crates/cli`. The CLI is a client and nothing else
//! (DOSSIER.md §4.1): every verb is a `/v1/` read projection or `POST /v1/commands/<Name>`,
//! it holds a bearer session and no other credential, and there is no flag that widens
//! anything — `api get` and `api command` are constrained to `/v1/` paths and catalogued
//! command names. Policy, RBAC and approvals are the server's; a refusal is printed as the
//! §15 error it is.
//!
//! This module is the pure half — the argument grammar, the credential store, the path
//! rule, the digest check and the output shaping — so it can be tested without a server.
//! `main.rs` is the transport.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

/// Repository path of this crate's canonical owner.
pub const CANONICAL_OWNER: &str = "crates/cli";
/// The CLI's version, from the crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The public API prefix every request is under.
pub const PUBLIC_PREFIX: &str = "/v1/";
/// Where the credential file lives unless `QUANSIO_CLI_HOME` says otherwise.
pub const DEFAULT_HOME: &str = ".config/quansio";
/// The credential file name.
pub const CREDENTIALS_FILE: &str = "credentials.json";

/// The nouns and verbs the CLI understands, each a public projection or command.
pub const VERBS: &[(&str, &[&str])] = &[
    ("login", &[]),
    ("logout", &[]),
    ("whoami", &[]),
    ("workspaces", &["list", "use", "create"]),
    ("threads", &["list"]),
    ("messages", &["list", "post"]),
    ("objectives", &["create"]),
    ("runs", &["get", "timeline", "cancel"]),
    ("approvals", &["list", "show", "approve", "deny"]),
    ("artifacts", &["list", "versions", "download"]),
    ("routines", &["list"]),
    ("targets", &["list"]),
    ("diagnostics", &["bundle"]),
    ("api", &["get", "command"]),
];

/// The usage text.
pub const USAGE: &str =
    "usage: quansio [--server URL] [--tenant tn_…] [--json] <noun> <verb> [options]

  login      --email E [--name N]        request a sign-in link (delivered by the server)
  login      --token T                    complete the sign-in with the link's token
  logout                                  end the session and forget it
  whoami                                  the session's user, tenant and workspaces
  workspaces list | use <ws_> | create --name N
  threads    list
  messages   list --thread <thr_> | post --content C [--thread <thr_>] [--teammate <agt_>]
  objectives create --title T [--description D]
  runs       get <run_> | timeline <run_> | cancel <run_>
  approvals  list [--run <run_>] | show <apr_> | approve <apr_> --digest <sha256> | deny <apr_>
  artifacts  list | versions <art_> | download <art_> <artv_> --out FILE
  routines   list
  targets    list
  diagnostics bundle [--out FILE]
  api        get </v1/…> | command <Name> [--params JSON]

Every call is a public API v1 call under the session's tenant; nothing bypasses policy.";

/// What one invocation asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// `--server`: the origin to talk to (else the stored session's).
    pub server: Option<String>,
    /// `--tenant`: the tenant the session must be scoped to (else the stored session's).
    pub tenant: Option<String>,
    /// `--json`: print the server's payload rather than a summary.
    pub json: bool,
    /// The verb.
    pub command: Command,
}

/// One verb with its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `login --email E [--name N]` or `login --token T`.
    Login {
        /// The address to send the link to.
        email: Option<String>,
        /// Display name for a first sign-in.
        name: Option<String>,
        /// The token from the link.
        token: Option<String>,
    },
    /// `logout`.
    Logout,
    /// `whoami`.
    Whoami,
    /// `workspaces list`.
    WorkspacesList,
    /// `workspaces use <ws_>`.
    WorkspacesUse {
        /// The workspace to make the default.
        workspace_id: String,
    },
    /// `workspaces create --name N`.
    WorkspacesCreate {
        /// The name.
        name: String,
    },
    /// `threads list`.
    ThreadsList,
    /// `messages list --thread <thr_>`.
    MessagesList {
        /// The thread.
        thread_id: String,
    },
    /// `messages post --content C [--thread] [--teammate]`.
    MessagesPost {
        /// The text.
        content: String,
        /// The thread, or a new one.
        thread_id: Option<String>,
        /// The teammate to address.
        teammate_id: Option<String>,
    },
    /// `objectives create --title T [--description D]`.
    ObjectivesCreate {
        /// Title.
        title: String,
        /// Description.
        description: Option<String>,
    },
    /// `runs get <run_>`.
    RunsGet {
        /// The run.
        run_id: String,
    },
    /// `runs timeline <run_>`.
    RunsTimeline {
        /// The run.
        run_id: String,
    },
    /// `runs cancel <run_>`.
    RunsCancel {
        /// The run.
        run_id: String,
    },
    /// `approvals list [--run <run_>]`.
    ApprovalsList {
        /// Only this run's.
        run_id: Option<String>,
    },
    /// `approvals show <apr_>`: the preview and the digest a decision binds to.
    ApprovalsShow {
        /// The request.
        approval_id: String,
    },
    /// `approvals approve <apr_> --digest <sha256>`: bound to the digest that was previewed.
    ApprovalsApprove {
        /// The request.
        approval_id: String,
        /// The previewed digest.
        digest: String,
    },
    /// `approvals deny <apr_>`.
    ApprovalsDeny {
        /// The request.
        approval_id: String,
    },
    /// `artifacts list`.
    ArtifactsList,
    /// `artifacts versions <art_>`.
    ArtifactsVersions {
        /// The artifact.
        artifact_id: String,
    },
    /// `artifacts download <art_> <artv_> --out FILE`: bytes verified against the digest.
    ArtifactsDownload {
        /// The artifact.
        artifact_id: String,
        /// The version.
        version_id: String,
        /// Where to write.
        out: PathBuf,
    },
    /// `routines list`.
    RoutinesList,
    /// `targets list`.
    TargetsList,
    /// `diagnostics bundle [--out FILE]`.
    DiagnosticsBundle {
        /// Where to write; stdout when absent.
        out: Option<PathBuf>,
    },
    /// `api get </v1/…>`.
    ApiGet {
        /// The public path.
        path: String,
    },
    /// `api command <Name> [--params JSON]`.
    ApiCommand {
        /// The catalogued command name.
        name: String,
        /// Its params.
        params: Value,
    },
}

/// A usage error: the invocation is not one the grammar accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

fn usage(message: impl Into<String>) -> UsageError {
    UsageError(message.into())
}

/// Options after the noun/verb: `--key value` pairs and positional words.
#[derive(Debug, Default)]
struct Options {
    flags: BTreeMap<String, String>,
    positional: Vec<String>,
}

fn options(words: &[String]) -> Result<Options, UsageError> {
    let mut parsed = Options::default();
    let mut index = 0;
    while index < words.len() {
        let word = &words[index];
        if let Some(key) = word.strip_prefix("--") {
            let value = words
                .get(index + 1)
                .ok_or_else(|| usage(format!("--{key} needs a value")))?;
            parsed.flags.insert(key.to_string(), value.clone());
            index += 2;
        } else {
            parsed.positional.push(word.clone());
            index += 1;
        }
    }
    Ok(parsed)
}

fn prefixed(value: &str, prefix: &str, what: &str) -> Result<String, UsageError> {
    if value.starts_with(prefix) && value.len() > prefix.len() {
        Ok(value.to_string())
    } else {
        Err(usage(format!(
            "{what} must be a {prefix}… id, not {value:?}"
        )))
    }
}

/// Parse `args` (without the program name).
///
/// # Errors
/// Returns [`UsageError`] naming what is missing or unknown; a flag that would bypass
/// policy is refused by name, not treated as a typo.
pub fn parse(args: &[String]) -> Result<Invocation, UsageError> {
    let mut server = None;
    let mut tenant = None;
    let mut json = false;
    let mut rest: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => json = true,
            "--server" => {
                server = Some(
                    args.get(index + 1)
                        .cloned()
                        .ok_or_else(|| usage("--server needs a URL"))?,
                );
                index += 1;
            }
            "--tenant" => {
                let value = args
                    .get(index + 1)
                    .cloned()
                    .ok_or_else(|| usage("--tenant needs a tn_… id"))?;
                tenant = Some(prefixed(&value, "tn_", "--tenant")?);
                index += 1;
            }
            "--force" | "--bypass-policy" | "--no-policy" | "--sudo" | "--admin" => {
                return Err(usage(format!(
                    "{} is not a flag: the CLI never bypasses API or runtime policy",
                    args[index]
                )));
            }
            "-h" | "--help" | "help" => return Err(usage(USAGE)),
            other => rest.push(other.to_string()),
        }
        index += 1;
    }
    let Some(noun) = rest.first().cloned() else {
        return Err(usage(USAGE));
    };
    let known = VERBS
        .iter()
        .find(|(name, _)| *name == noun)
        .ok_or_else(|| usage(format!("unknown noun {noun:?}\n\n{USAGE}")))?;
    let (verb, words) = if known.1.is_empty() {
        (None, &rest[1..])
    } else {
        let verb = rest
            .get(1)
            .cloned()
            .ok_or_else(|| usage(format!("{noun} needs one of {:?}", known.1)))?;
        if !known.1.contains(&verb.as_str()) {
            return Err(usage(format!(
                "{noun} {verb}: unknown verb; expected one of {:?}",
                known.1
            )));
        }
        (Some(verb), &rest[2..])
    };
    let opts = options(words)?;
    let flag = |key: &str| opts.flags.get(key).cloned();
    let required_flag = |key: &str| {
        flag(key).ok_or_else(|| {
            usage(format!(
                "{noun} {}: --{key} is required",
                verb.as_deref().unwrap_or("")
            ))
        })
    };
    let positional = |at: usize, what: &str| {
        opts.positional.get(at).cloned().ok_or_else(|| {
            usage(format!(
                "{noun} {}: {what} is required",
                verb.as_deref().unwrap_or("")
            ))
        })
    };
    let command = match (noun.as_str(), verb.as_deref()) {
        ("login", None) => {
            let email = flag("email");
            let token = flag("token");
            if email.is_none() && token.is_none() {
                return Err(usage(
                    "login needs --email (to request a link) or --token (to complete it)",
                ));
            }
            Command::Login {
                email,
                name: flag("name"),
                token,
            }
        }
        ("logout", None) => Command::Logout,
        ("whoami", None) => Command::Whoami,
        ("workspaces", Some("list")) => Command::WorkspacesList,
        ("workspaces", Some("use")) => Command::WorkspacesUse {
            workspace_id: prefixed(&positional(0, "workspace id")?, "ws_", "workspace id")?,
        },
        ("workspaces", Some("create")) => Command::WorkspacesCreate {
            name: required_flag("name")?,
        },
        ("threads", Some("list")) => Command::ThreadsList,
        ("messages", Some("list")) => Command::MessagesList {
            thread_id: prefixed(&required_flag("thread")?, "thr_", "--thread")?,
        },
        ("messages", Some("post")) => Command::MessagesPost {
            content: required_flag("content")?,
            thread_id: flag("thread")
                .map(|value| prefixed(&value, "thr_", "--thread"))
                .transpose()?,
            teammate_id: flag("teammate")
                .map(|value| prefixed(&value, "agt_", "--teammate"))
                .transpose()?,
        },
        ("objectives", Some("create")) => Command::ObjectivesCreate {
            title: required_flag("title")?,
            description: flag("description"),
        },
        ("runs", Some("get")) => Command::RunsGet {
            run_id: prefixed(&positional(0, "run id")?, "run_", "run id")?,
        },
        ("runs", Some("timeline")) => Command::RunsTimeline {
            run_id: prefixed(&positional(0, "run id")?, "run_", "run id")?,
        },
        ("runs", Some("cancel")) => Command::RunsCancel {
            run_id: prefixed(&positional(0, "run id")?, "run_", "run id")?,
        },
        ("approvals", Some("list")) => Command::ApprovalsList {
            run_id: flag("run")
                .map(|value| prefixed(&value, "run_", "--run"))
                .transpose()?,
        },
        ("approvals", Some("show")) => Command::ApprovalsShow {
            approval_id: prefixed(&positional(0, "approval id")?, "apr_", "approval id")?,
        },
        ("approvals", Some("approve")) => {
            let digest = required_flag("digest")?;
            if !is_sha256_hex(&digest) {
                return Err(usage("approvals approve: --digest must be the previewed sha256 hex digest (64 characters)"));
            }
            Command::ApprovalsApprove {
                approval_id: prefixed(&positional(0, "approval id")?, "apr_", "approval id")?,
                digest,
            }
        }
        ("approvals", Some("deny")) => Command::ApprovalsDeny {
            approval_id: prefixed(&positional(0, "approval id")?, "apr_", "approval id")?,
        },
        ("artifacts", Some("list")) => Command::ArtifactsList,
        ("artifacts", Some("versions")) => Command::ArtifactsVersions {
            artifact_id: prefixed(&positional(0, "artifact id")?, "art_", "artifact id")?,
        },
        ("artifacts", Some("download")) => Command::ArtifactsDownload {
            artifact_id: prefixed(&positional(0, "artifact id")?, "art_", "artifact id")?,
            version_id: prefixed(&positional(1, "version id")?, "artv_", "version id")?,
            out: PathBuf::from(required_flag("out")?),
        },
        ("routines", Some("list")) => Command::RoutinesList,
        ("targets", Some("list")) => Command::TargetsList,
        ("diagnostics", Some("bundle")) => Command::DiagnosticsBundle {
            out: flag("out").map(PathBuf::from),
        },
        ("api", Some("get")) => Command::ApiGet {
            path: public_path(&positional(0, "path")?)?,
        },
        ("api", Some("command")) => {
            let name = positional(0, "command name")?;
            if !is_command_name(&name) {
                return Err(usage(format!("api command: {name:?} is not a command name (PascalCase, as the catalog spells it)")));
            }
            let params = match flag("params") {
                Some(text) => serde_json::from_str::<Value>(&text)
                    .map_err(|error| usage(format!("--params is not JSON: {error}")))?,
                None => json!({}),
            };
            if !params.is_object() {
                return Err(usage("--params must be a JSON object"));
            }
            Command::ApiCommand { name, params }
        }
        _ => return Err(usage(USAGE)),
    };
    Ok(Invocation {
        server,
        tenant,
        json,
        command,
    })
}

/// A public API path: `/v1/…`, one segment or more, no scheme, no `..`, no fragment.
///
/// # Errors
/// Returns [`UsageError`] for anything else — there is no other prefix the CLI addresses.
pub fn public_path(path: &str) -> Result<String, UsageError> {
    let trimmed = path.trim();
    if !trimmed.starts_with(PUBLIC_PREFIX) || trimmed.len() <= PUBLIC_PREFIX.len() {
        return Err(usage(format!(
            "{path:?} is not a public API path (every path is under {PUBLIC_PREFIX})"
        )));
    }
    if trimmed.contains("..")
        || trimmed.contains("://")
        || trimmed.contains('#')
        || trimmed.contains(char::is_whitespace)
    {
        return Err(usage(format!("{path:?} is not a public API path")));
    }
    Ok(trimmed.to_string())
}

/// Whether `name` is spelled like a catalogued command (`PascalCase`).
#[must_use]
pub fn is_command_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_uppercase())
        && name.len() >= 3
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Whether `value` is a bare sha256 hex digest.
#[must_use]
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// The sha256 hex of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Whether `bytes` hash to `digest`.
#[must_use]
pub fn verify_digest(bytes: &[u8], digest: &str) -> bool {
    sha256_hex(bytes) == digest
}

// ------------------------------------------------------------------ credentials

/// One stored session, bound to the server origin and the tenant it acts in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSession {
    /// The server origin (`https://…`, no trailing slash).
    pub origin: String,
    /// The tenant the tokens are scoped to.
    pub tenant_id: String,
    /// The user.
    pub user_id: String,
    /// The default workspace for verbs that need one.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// The bearer token.
    pub access_token: String,
    /// When it stops verifying.
    pub access_expires_at: String,
    /// The rotating refresh token.
    pub refresh_token: String,
    /// When it stops rotating.
    pub refresh_expires_at: String,
}

impl StoredSession {
    /// The session with every secret replaced by its scope and expiry.
    #[must_use]
    pub fn redacted(&self) -> Value {
        json!({
            "origin": self.origin,
            "tenant_id": self.tenant_id,
            "user_id": self.user_id,
            "workspace_id": self.workspace_id,
            "access_token": "<redacted>",
            "access_expires_at": self.access_expires_at,
            "refresh_token": "<redacted>",
            "refresh_expires_at": self.refresh_expires_at,
        })
    }
}

/// The credential file: every stored session, one per `(origin, tenant)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// Stored sessions.
    #[serde(default)]
    pub sessions: Vec<StoredSession>,
}

/// A credential error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    /// No session for the requested scope.
    NoSession(String),
    /// The requested scope does not match the stored session.
    ScopeMismatch(String),
    /// The file could not be read or written.
    Io(String),
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSession(message) | Self::ScopeMismatch(message) | Self::Io(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for CredentialError {}

/// Normalize an origin: scheme + host[:port], no path, no trailing slash.
#[must_use]
pub fn normalize_origin(origin: &str) -> String {
    origin.trim().trim_end_matches('/').to_string()
}

/// The credential directory: `$QUANSIO_CLI_HOME`, else `$HOME/.config/quansio`.
#[must_use]
pub fn home_dir(env_home: Option<&str>, user_home: Option<&str>) -> PathBuf {
    match env_home.filter(|value| !value.is_empty()) {
        Some(explicit) => PathBuf::from(explicit),
        None => PathBuf::from(user_home.unwrap_or(".")).join(DEFAULT_HOME),
    }
}

impl Credentials {
    /// Load the file under `home`; a missing file is an empty store.
    ///
    /// # Errors
    /// [`CredentialError::Io`] when the file exists and cannot be read or parsed.
    pub fn load(home: &Path) -> Result<Self, CredentialError> {
        let path = home.join(CREDENTIALS_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| CredentialError::Io(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(CredentialError::Io(format!("{}: {error}", path.display()))),
        }
    }

    /// Write the file under `home`, readable by the owner only.
    ///
    /// # Errors
    /// [`CredentialError::Io`].
    pub fn save(&self, home: &Path) -> Result<PathBuf, CredentialError> {
        std::fs::create_dir_all(home)
            .map_err(|error| CredentialError::Io(format!("{}: {error}", home.display())))?;
        let path = home.join(CREDENTIALS_FILE);
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| CredentialError::Io(error.to_string()))?;
        std::fs::write(&path, bytes)
            .map_err(|error| CredentialError::Io(format!("{}: {error}", path.display())))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| CredentialError::Io(format!("{}: {error}", path.display())))?;
        }
        Ok(path)
    }

    /// Store a session, replacing the one for the same `(origin, tenant)`.
    pub fn put(&mut self, session: StoredSession) {
        self.sessions.retain(|existing| {
            !(existing.origin == session.origin && existing.tenant_id == session.tenant_id)
        });
        self.sessions.push(session);
    }

    /// Forget the session for `(origin, tenant)`; whether one was there.
    pub fn remove(&mut self, origin: &str, tenant_id: &str) -> bool {
        let before = self.sessions.len();
        self.sessions
            .retain(|existing| !(existing.origin == origin && existing.tenant_id == tenant_id));
        before != self.sessions.len()
    }

    /// The session an invocation acts with: the one for `--server`/`--tenant` when given,
    /// else the only one; a requested scope that matches no stored session is a refusal,
    /// never a fallback to another tenant's token.
    ///
    /// # Errors
    /// [`CredentialError::NoSession`] when nothing is stored for the scope,
    /// [`CredentialError::ScopeMismatch`] when the scope names a different session than
    /// the stored one(s) could satisfy.
    pub fn select(
        &self,
        server: Option<&str>,
        tenant: Option<&str>,
    ) -> Result<&StoredSession, CredentialError> {
        let origin = server.map(normalize_origin);
        let matching: Vec<&StoredSession> = self
            .sessions
            .iter()
            .filter(|session| {
                origin
                    .as_deref()
                    .is_none_or(|wanted| session.origin == wanted)
            })
            .filter(|session| tenant.is_none_or(|wanted| session.tenant_id == wanted))
            .collect();
        match matching.as_slice() {
            [session] => Ok(session),
            [] if self.sessions.is_empty() => Err(CredentialError::NoSession(
                "not signed in: run `quansio login --server URL --email you@example.com`"
                    .to_string(),
            )),
            [] => Err(CredentialError::ScopeMismatch(format!(
                "no session for {}{}; stored sessions: {}",
                origin
                    .as_deref()
                    .map(|value| format!("server {value}"))
                    .unwrap_or_else(|| "any server".to_string()),
                tenant
                    .map(|value| format!(" and tenant {value}"))
                    .unwrap_or_default(),
                self.sessions
                    .iter()
                    .map(|session| format!("{} ({})", session.origin, session.tenant_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            _ => Err(CredentialError::ScopeMismatch(format!(
                "several sessions match; say which with --server and --tenant: {}",
                matching
                    .iter()
                    .map(|session| format!("{} ({})", session.origin, session.tenant_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }
}

/// The diagnostics bundle: what an operator can share without sharing a secret.
#[must_use]
pub fn diagnostics_bundle(
    session: Option<&StoredSession>,
    health: Option<&Value>,
    commands: Option<&Value>,
    projections: Option<&Value>,
    me: Option<&Value>,
    errors: &[String],
) -> Value {
    let mut me = me.cloned().unwrap_or(Value::Null);
    if let Some(object) = me.as_object_mut() {
        object.remove("access_token");
        object.remove("refresh_token");
    }
    json!({
        "cli_version": VERSION,
        "recorded_at": chrono::Utc::now().to_rfc3339(),
        "session": session.map(StoredSession::redacted),
        "server": { "health": health, "commands": commands, "read_projections": projections },
        "me": me,
        "errors": errors,
    })
}

/// Redact anything that looks like a bearer or refresh token in a payload before printing.
#[must_use]
pub fn redact_tokens(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, inner)| {
                    if key.ends_with("_token") || key == "token" {
                        (key.clone(), json!("<redacted>"))
                    } else {
                        (key.clone(), redact_tokens(inner))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_tokens).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn every_verb_parses_to_a_public_call_and_nothing_bypasses_policy() {
        let cases = [
            "login --email a@b.co --name Ada",
            "login --token qml_x",
            "logout",
            "whoami",
            "workspaces list",
            "workspaces use ws_1",
            "workspaces create --name Ops",
            "threads list",
            "messages list --thread thr_1",
            "messages post --content hi --thread thr_1 --teammate agt_1",
            "objectives create --title ship --description now",
            "runs get run_1",
            "runs timeline run_1",
            "runs cancel run_1",
            "approvals list --run run_1",
            "approvals show apr_1",
            "approvals deny apr_1",
            "artifacts list",
            "artifacts versions art_1",
            "artifacts download art_1 artv_1 --out /tmp/x",
            "routines list",
            "targets list",
            "diagnostics bundle --out /tmp/d.json",
            "api get /v1/audit",
        ];
        for case in cases {
            parse(&args(case)).unwrap_or_else(|error| panic!("{case}: {error}"));
        }
        let approve = parse(&args(&format!(
            "approvals approve apr_1 --digest {}",
            "a".repeat(64)
        )))
        .expect("approve");
        assert!(
            matches!(approve.command, Command::ApprovalsApprove { ref digest, .. } if digest.len() == 64)
        );
        assert!(parse(&args("approvals approve apr_1 --digest short")).is_err());
        let global = parse(&args(
            "--json --server https://q.example/ --tenant tn_1 whoami",
        ))
        .expect("global flags");
        assert!(global.json);
        assert_eq!(global.server.as_deref(), Some("https://q.example/"));
        assert_eq!(global.tenant.as_deref(), Some("tn_1"));
        for bypass in [
            "--force whoami",
            "whoami --bypass-policy",
            "--no-policy runs get run_1",
            "--sudo approvals list",
        ] {
            let error = parse(&args(bypass)).expect_err(bypass);
            assert!(error.0.contains("never bypasses"), "{bypass}: {error}");
        }
        assert!(
            parse(&args("runs get thr_1")).is_err(),
            "a run id is a run_ id"
        );
        assert!(parse(&args("workspaces nuke")).is_err());
        assert!(parse(&args("")).is_err());
    }

    #[test]
    fn the_generic_verbs_are_constrained_to_the_public_api() {
        assert_eq!(public_path("/v1/members").expect("ok"), "/v1/members");
        for bad in [
            "/admin",
            "/v2/x",
            "/v1/",
            "v1/members",
            "/v1/../etc",
            "https://x/v1/a",
            "/v1/a b",
            "/v1/a#b",
        ] {
            assert!(public_path(bad).is_err(), "{bad}");
        }
        assert!(parse(&args("api get /internal/db")).is_err());
        assert!(parse(&args("api command setPolicy")).is_err());
        assert!(parse(&args("api command Set-Policy")).is_err());
        let ok = parse(&args(
            "api command SetPolicy --params {\"scope\":\"tenant\",\"rules\":[]}",
        ))
        .expect("command");
        assert!(
            matches!(ok.command, Command::ApiCommand { ref name, ref params } if name == "SetPolicy" && params["scope"] == "tenant")
        );
        assert!(parse(&args("api command SetPolicy --params [1]")).is_err());
        assert!(parse(&args("api command SetPolicy --params nope")).is_err());
    }

    fn session(origin: &str, tenant: &str) -> StoredSession {
        StoredSession {
            origin: origin.to_string(),
            tenant_id: tenant.to_string(),
            user_id: "usr_1".to_string(),
            workspace_id: Some("ws_1".to_string()),
            access_token: "secret-access".to_string(),
            access_expires_at: "2999-01-01T00:00:00Z".to_string(),
            refresh_token: "secret-refresh".to_string(),
            refresh_expires_at: "2999-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn credentials_are_scoped_to_server_and_tenant_and_stored_for_the_owner_only() {
        let home = tempfile::tempdir().expect("home");
        let mut store = Credentials::default();
        assert!(matches!(
            store.select(None, None),
            Err(CredentialError::NoSession(_))
        ));
        store.put(session("https://a.example", "tn_a"));
        store.put(session("https://b.example", "tn_b"));
        store.put(session("https://a.example", "tn_a")); // replaces, never duplicates
        assert_eq!(store.sessions.len(), 2);
        // Two sessions: the caller must say which.
        assert!(matches!(
            store.select(None, None),
            Err(CredentialError::ScopeMismatch(_))
        ));
        assert_eq!(
            store
                .select(Some("https://a.example/"), None)
                .expect("by server")
                .tenant_id,
            "tn_a"
        );
        assert_eq!(
            store.select(None, Some("tn_b")).expect("by tenant").origin,
            "https://b.example"
        );
        // A token is never used for a scope it was not issued for.
        assert!(matches!(
            store.select(Some("https://a.example"), Some("tn_b")),
            Err(CredentialError::ScopeMismatch(_))
        ));
        assert!(matches!(
            store.select(Some("https://c.example"), None),
            Err(CredentialError::ScopeMismatch(_))
        ));
        let path = store.save(home.path()).expect("save");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777,
                0o600
            );
        }
        let mut loaded = Credentials::load(home.path()).expect("load");
        assert_eq!(loaded, store);
        assert_eq!(
            Credentials::load(&home.path().join("nowhere")).expect("empty"),
            Credentials::default()
        );
        assert!(loaded.remove("https://a.example", "tn_a"));
        assert!(!loaded.remove("https://a.example", "tn_a"));
        // Secrets never reach the screen.
        let shown = loaded.sessions[0].redacted();
        assert_eq!(shown["access_token"], "<redacted>");
        assert!(!shown.to_string().contains("secret"));
        let bundle = diagnostics_bundle(
            Some(&loaded.sessions[0]),
            Some(&json!({"status":"ready"})),
            None,
            None,
            Some(&json!({"user": {"id": "usr_1"}, "access_token": "leak"})),
            &["x".to_string()],
        );
        assert!(!bundle.to_string().contains("leak") && !bundle.to_string().contains("secret"));
        assert_eq!(
            redact_tokens(
                &json!({"session": {"access_token": "a", "refresh_token": "b", "id": "s"}, "list": [{"token": "t"}]})
            ),
            json!({"session": {"access_token": "<redacted>", "refresh_token": "<redacted>", "id": "s"}, "list": [{"token": "<redacted>"}]})
        );
        assert_eq!(home_dir(Some("/x"), Some("/home/u")), PathBuf::from("/x"));
        assert_eq!(
            home_dir(None, Some("/home/u")),
            PathBuf::from("/home/u/.config/quansio")
        );
    }

    #[test]
    fn digests_are_checked_the_way_the_server_labels_them() {
        let digest = sha256_hex(b"all tests passed\n");
        assert!(is_sha256_hex(&digest));
        assert!(verify_digest(b"all tests passed\n", &digest));
        assert!(!verify_digest(b"tampered", &digest));
        assert!(!is_sha256_hex(&digest.to_uppercase()));
    }
}

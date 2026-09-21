//! Default teammate templates (APP-014): configuration, not code.
//!
//! `config/teammates.yaml` ships with the server (embedded, so a deployment cannot lose
//! it) and is parsed into typed [`Template`]s; an unparseable template is a startup
//! defect, not a fallback. `CreateTeammate {template_id}` instantiates one.

use serde::Deserialize;

use super::TeammateError;

/// One template as shipped.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Template {
    /// Stable id (`general-assistant`, …).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Persona text.
    #[serde(default)]
    pub persona: String,
    /// Standing instructions.
    #[serde(default)]
    pub standing_instructions: String,
    /// Effect classes the teammate needs (narrowing only).
    #[serde(default)]
    pub default_capability_needs: Vec<String>,
    /// `user | workspace | teammate`.
    #[serde(default = "workspace_scope")]
    pub memory_scope: String,
    /// Preferred model route, when any.
    #[serde(default)]
    pub model_route_preference: Option<String>,
    /// `available | away`.
    #[serde(default = "available")]
    pub availability: String,
}

fn workspace_scope() -> String {
    "workspace".to_string()
}

fn available() -> String {
    "available".to_string()
}

#[derive(Debug, Deserialize)]
struct TemplateFile {
    #[allow(dead_code)]
    schema_version: String,
    templates: Vec<Template>,
}

/// The shipped YAML.
#[must_use]
pub fn templates_yaml() -> &'static str {
    include_str!("../../../../../config/teammates.yaml")
}

/// Parse a template file.
///
/// # Errors
/// Returns [`TeammateError::Config`] when the file is not the shape `config/teammates.yaml` has.
pub fn parse_templates(yaml: &str) -> Result<Vec<Template>, TeammateError> {
    let file: TemplateFile =
        serde_yaml::from_str(yaml).map_err(|error| TeammateError::Config(error.to_string()))?;
    for template in &file.templates {
        if template.id.is_empty() || template.name.is_empty() {
            return Err(TeammateError::Config(
                "every template needs an id and a name".to_string(),
            ));
        }
        super::validate_memory_scope(&template.memory_scope)?;
        super::validate_availability(&template.availability)?;
    }
    Ok(file.templates)
}

/// The shipped templates.
///
/// # Errors
/// Returns [`TeammateError::Config`] when the shipped file is malformed.
pub fn templates() -> Result<Vec<Template>, TeammateError> {
    parse_templates(templates_yaml())
}

/// Template ids, for callers that only need the vocabulary.
#[must_use]
pub fn template_ids(yaml: &str) -> Vec<String> {
    parse_templates(yaml)
        .map(|templates| templates.into_iter().map(|template| template.id).collect())
        .unwrap_or_default()
}

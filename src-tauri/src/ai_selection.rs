use serde::{Deserialize, Serialize};

pub const PROVIDER_OPENCODE: &str = "opencode";
pub const PROVIDER_CASCADE: &str = "cascade";
pub const TYPESAFE_DEFAULT_MODEL: &str = "jev-latest";
pub const DEFAULT_CASCADE_CONFIDENCE: f64 = 0.8;
pub const DEFAULT_GAP_NOUL_THRESHOLD: f64 = 0.55;
/// Paid subcategory floor for high-value segments (schools, PA, orgs, …).
pub const DEFAULT_SUBCATEGORY_FLOOR: f64 = 0.70;
/// Above this, skip subcategory even on high-value (category-only apply).
pub const DEFAULT_CHEAP_SKIP_SUB_ABOVE: f64 = 0.88;
pub const DEFAULT_OPENCODE_ESCALATION_MODEL: &str = "opencode-go/muse-spark-1.3-contributor";
pub const DEFAULT_OPENCODE_ESCALATION_EFFORT: &str = "xhigh";
/// Third cascade rung after Jev → Muse Spark: retry only on Muse Spark at `xhigh`.
pub const DEFAULT_OPENCODE_RETRY_EFFORT: &str = "xhigh";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderSelection {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub escalation_provider: Option<String>,
    #[serde(default)]
    pub escalation_model: Option<String>,
    #[serde(default)]
    pub escalation_effort: Option<String>,
    #[serde(default = "default_jev_model")]
    pub jev_model: String,
    #[serde(default = "default_confidence")]
    pub confidence_threshold: f64,
    #[serde(default = "default_gap_threshold")]
    pub gap_noul_threshold: f64,
    #[serde(default = "default_subcategory_floor")]
    pub subcategory_floor: f64,
    #[serde(default = "default_cheap_skip_sub")]
    pub cheap_skip_sub_above: f64,
}

fn default_jev_model() -> String {
    TYPESAFE_DEFAULT_MODEL.to_owned()
}

fn default_confidence() -> f64 {
    DEFAULT_CASCADE_CONFIDENCE
}

fn default_gap_threshold() -> f64 {
    DEFAULT_GAP_NOUL_THRESHOLD
}

fn default_subcategory_floor() -> f64 {
    DEFAULT_SUBCATEGORY_FLOOR
}

fn default_cheap_skip_sub() -> f64 {
    DEFAULT_CHEAP_SKIP_SUB_ABOVE
}

impl Default for ProviderSelection {
    fn default() -> Self {
        Self {
            provider: PROVIDER_CASCADE.to_owned(),
            model: TYPESAFE_DEFAULT_MODEL.to_owned(),
            effort: None,
            escalation_provider: Some(PROVIDER_OPENCODE.to_owned()),
            escalation_model: Some(DEFAULT_OPENCODE_ESCALATION_MODEL.to_owned()),
            escalation_effort: Some(DEFAULT_OPENCODE_ESCALATION_EFFORT.to_owned()),
            jev_model: default_jev_model(),
            confidence_threshold: default_confidence(),
            gap_noul_threshold: default_gap_threshold(),
            subcategory_floor: default_subcategory_floor(),
            cheap_skip_sub_above: default_cheap_skip_sub(),
        }
    }
}

impl ProviderSelection {
    pub fn normalize(mut self) -> Result<Self, String> {
        self.provider = self.provider.trim().to_ascii_lowercase();
        match self.provider.as_str() {
            PROVIDER_OPENCODE | PROVIDER_CASCADE => {}
            other => return Err(format!("Provider IA non supportato: {other}")),
        }
        if self.provider == PROVIDER_CASCADE {
            let escalation_provider = self
                .escalation_provider
                .get_or_insert_with(|| PROVIDER_OPENCODE.to_owned());
            *escalation_provider = escalation_provider.trim().to_ascii_lowercase();
            if escalation_provider.as_str() != PROVIDER_OPENCODE {
                return Err(format!(
                    "Provider di escalation non supportato: {escalation_provider}"
                ));
            }
            if self
                .escalation_model
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                self.escalation_model = Some(DEFAULT_OPENCODE_ESCALATION_MODEL.to_owned());
            }
            if self
                .escalation_effort
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                self.escalation_effort = Some(DEFAULT_OPENCODE_ESCALATION_EFFORT.to_owned());
            }
            if self.model.trim().is_empty() {
                self.model = self.jev_model.clone();
            }
            if !(0.0..=1.0).contains(&self.confidence_threshold) {
                return Err("Soglia di confidenza Jev fuori intervallo 0..1".to_owned());
            }
            if !(0.0..=1.0).contains(&self.gap_noul_threshold) {
                return Err("Soglia della lacuna tassonomica fuori dall'intervallo 0..1".to_owned());
            }
            if !(0.0..=1.0).contains(&self.subcategory_floor) {
                return Err("Soglia sottocategoria fuori intervallo 0..1".to_owned());
            }
            if !(0.0..=1.0).contains(&self.cheap_skip_sub_above) {
                return Err("Soglia skip sottocategoria fuori intervallo 0..1".to_owned());
            }
            if self.subcategory_floor > self.confidence_threshold {
                return Err(
                    "Soglia sottocategoria non può superare la soglia di confidenza".to_owned(),
                );
            }
            if self.cheap_skip_sub_above < self.confidence_threshold {
                return Err(
                    "Soglia skip sottocategoria non può essere sotto la soglia di confidenza"
                        .to_owned(),
                );
            }
        } else if self.model.trim().is_empty() {
            return Err("Modello IA obbligatorio".to_owned());
        }
        Ok(self)
    }

    pub fn spend_policy(&self) -> crate::ai_typesafe::SpendPolicy {
        crate::ai_typesafe::SpendPolicy {
            apply_confidence: self.confidence_threshold,
            subcategory_floor: self.subcategory_floor,
            cheap_skip_sub_above: self.cheap_skip_sub_above,
            gap_noul_threshold: self.gap_noul_threshold,
        }
    }

    pub fn generative_provider_id(&self) -> &str {
        if self.provider == PROVIDER_CASCADE {
            self.escalation_provider
                .as_deref()
                .unwrap_or(PROVIDER_OPENCODE)
        } else {
            self.provider.as_str()
        }
    }

    pub fn generative_model(&self) -> &str {
        if self.provider == PROVIDER_CASCADE {
            self.escalation_model
                .as_deref()
                .unwrap_or(self.model.as_str())
        } else {
            self.model.as_str()
        }
    }

    pub fn generative_effort(&self) -> Option<&str> {
        if self.provider == PROVIDER_CASCADE {
            self.escalation_effort.as_deref()
        } else {
            self.effort.as_deref()
        }
    }

    /// Effort for cascade structural retry (Muse Spark xhigh). Non-cascade keeps generative effort.
    pub fn generative_retry_effort(&self) -> Option<&str> {
        if self.provider == PROVIDER_CASCADE && self.generative_provider_id() == PROVIDER_OPENCODE {
            Some(DEFAULT_OPENCODE_RETRY_EFFORT)
        } else {
            self.generative_effort()
        }
    }

    pub fn run_model_label(&self) -> String {
        if self.provider == PROVIDER_CASCADE {
            format!(
                "cascade/{}+{}",
                self.jev_model,
                cascade_generative_label(self.generative_provider_id(), self.generative_model())
            )
        } else {
            self.model.clone()
        }
    }
}

/// Compose the `provider/model` segment of a cascade run label without doubling
/// the provider namespace.
///
/// The generative model may already carry its own catalog namespace: OpenCode stores ids such as
/// `opencode-go/muse-spark-1.3-contributor`. That namespace is part of the CLI model id,
/// so it is kept as-is; only a bare model gets the app provider prefix.
fn cascade_generative_label(provider: &str, model: &str) -> String {
    let model = model.trim();
    if model.contains('/') {
        model.to_owned()
    } else {
        format!("{provider}/{model}")
    }
}

/// Strip the app provider prefix from a model id.
///
/// The fallbacks intentionally cover the app provider ids (`opencode`), not
/// CLI catalog namespaces: `opencode-go/muse-spark-1.3-contributor` is a model namespace
/// owned by the catalog, and stripping it would make `OpenCodeProvider` re-qualify
/// the bare id as `opencode/muse-spark-1.3-contributor` (the wrong provider). Pass
/// `"opencode-go"` as `provider` explicitly when the namespace really must go.
pub(crate) fn strip_provider_prefix(provider: &str, model: &str) -> String {
    let prefixes = [format!("{provider}/"), format!("{PROVIDER_OPENCODE}/")];
    for prefix in prefixes {
        if let Some(rest) = model.strip_prefix(&prefix)
            && !rest.is_empty()
        {
            return rest.to_owned();
        }
    }
    model.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cascade_defaults_escalation_to_opencode_muse_spark() {
        assert_eq!(
            DEFAULT_OPENCODE_ESCALATION_MODEL,
            "opencode-go/muse-spark-1.3-contributor"
        );
        assert_eq!(DEFAULT_OPENCODE_ESCALATION_EFFORT, "xhigh");
        let selection = ProviderSelection {
            provider: PROVIDER_CASCADE.to_owned(),
            model: String::new(),
            escalation_provider: None,
            escalation_model: None,
            escalation_effort: None,
            ..ProviderSelection::default()
        }
        .normalize()
        .unwrap();
        assert_eq!(selection.generative_provider_id(), PROVIDER_OPENCODE);
        assert_eq!(
            selection.escalation_model.as_deref(),
            Some(DEFAULT_OPENCODE_ESCALATION_MODEL)
        );
        assert_eq!(
            selection.escalation_effort.as_deref(),
            Some(DEFAULT_OPENCODE_ESCALATION_EFFORT)
        );
        assert_eq!(
            selection.generative_retry_effort(),
            Some(DEFAULT_OPENCODE_RETRY_EFFORT)
        );
        // Both rungs resolve to `high` now: the retry is a fresh single-contact
        // call, not an effort bump. `minimal` is not a valid Muse Spark variant.
        assert_eq!(selection.generative_effort(), Some("xhigh"));
        assert_eq!(selection.generative_retry_effort(), Some("xhigh"));
        let label = selection.run_model_label();
        assert!(label.starts_with("cascade/"));
        assert_eq!(
            label,
            "cascade/jev-latest+opencode-go/muse-spark-1.3-contributor"
        );
        assert!(!label.contains("opencode/opencode-go"));
    }

    #[test]
    fn run_model_label_prefixes_bare_generative_model() {
        let selection = ProviderSelection {
            provider: PROVIDER_CASCADE.to_owned(),
            escalation_provider: Some(PROVIDER_OPENCODE.to_owned()),
            escalation_model: Some("muse-spark-1.3-contributor".to_owned()),
            escalation_effort: Some(DEFAULT_OPENCODE_ESCALATION_EFFORT.to_owned()),
            ..ProviderSelection::default()
        }
        .normalize()
        .unwrap();
        assert_eq!(
            selection.run_model_label(),
            "cascade/jev-latest+opencode/muse-spark-1.3-contributor"
        );
    }

    #[test]
    fn strip_provider_prefix_preserves_model_namespace() {
        // `opencode-go/` is the CLI catalog namespace of the Muse Spark model, not
        // the app provider id: keeping it lets OpenCode invoke the right model.
        assert_eq!(
            strip_provider_prefix(PROVIDER_OPENCODE, "opencode-go/muse-spark-1.3-contributor"),
            "opencode-go/muse-spark-1.3-contributor"
        );
        // An explicit namespace still strips down to the bare model id.
        assert_eq!(
            strip_provider_prefix("opencode-go", "opencode-go/muse-spark-1.3-contributor"),
            "muse-spark-1.3-contributor"
        );
        assert_eq!(
            strip_provider_prefix(PROVIDER_OPENCODE, "opencode/muse-spark-1.3-contributor"),
            "muse-spark-1.3-contributor"
        );
    }
}

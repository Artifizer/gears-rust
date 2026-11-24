use serde::{Deserialize, Serialize};

/// Configuration for the `always-allow-plugin` gear.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlwaysAllowPluginConfig {
    #[serde(default = "default_vendor")]
    pub vendor: String,
    #[serde(default = "default_priority")]
    pub priority: i16,
}

impl Default for AlwaysAllowPluginConfig {
    fn default() -> Self {
        Self {
            vendor: default_vendor(),
            priority: default_priority(),
        }
    }
}

fn default_vendor() -> String {
    "cf.builtin.always_allow".to_owned()
}

fn default_priority() -> i16 {
    1000
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_the_defaults() {
        let cfg: AlwaysAllowPluginConfig = serde_json::from_str("{}").expect("valid config");
        assert_eq!(cfg.vendor, "cf.builtin.always_allow");
        assert_eq!(cfg.priority, 1000);
        let dflt = AlwaysAllowPluginConfig::default();
        assert_eq!((dflt.vendor, dflt.priority), (cfg.vendor, cfg.priority));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<AlwaysAllowPluginConfig>(r#"{"vendr": "x"}"#).is_err());
    }
}

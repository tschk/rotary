use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCapsule {
    pub system_prompt: Option<String>,
    pub allowed_tools: Vec<String>,
    pub files: Vec<String>,
}

impl ContextCapsule {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn inherits_ambient(&self) -> bool {
        false
    }

    pub fn merge_parent(&self, _parent_system: Option<&str>, _parent_tools: &[String]) -> Self {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_ambient_inheritance() {
        let capsule = ContextCapsule {
            system_prompt: Some("child only".into()),
            allowed_tools: vec!["read".into()],
            files: vec!["src/lib.rs".into()],
        };
        let merged = capsule.merge_parent(Some("PARENT SYSTEM"), &["bash".into(), "write".into()]);
        assert_eq!(merged.system_prompt.as_deref(), Some("child only"));
        assert_eq!(merged.allowed_tools, vec!["read".to_string()]);
        assert!(!merged.allowed_tools.contains(&"bash".to_string()));
        assert!(!capsule.inherits_ambient());
    }

    #[test]
    fn empty_capsule_stays_empty() {
        let merged = ContextCapsule::empty().merge_parent(Some("parent"), &["read".into()]);
        assert!(merged.system_prompt.is_none());
        assert!(merged.allowed_tools.is_empty());
    }
}

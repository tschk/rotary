//! Memory recall/capture policy resolved over workspace layers.

use serde::{Deserialize, Serialize};

/// Which layers a turn may recall memories from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallMode {
    /// Recall nothing.
    Off,
    /// Recall only from the writable layer.
    Writable,
    /// Recall from every visible layer.
    Visible,
}

/// Whether a turn may write memories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Capture nothing.
    Off,
    /// Capture into the writable layer.
    Writable,
}

impl RecallMode {
    /// Parse a recall mode, falling back to the default for unknown values.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("off") => Self::Off,
            Some("writable") => Self::Writable,
            Some("visible") => Self::Visible,
            _ => MemoryPolicy::DEFAULT.recall,
        }
    }
}

impl CaptureMode {
    /// Parse a capture mode, falling back to the default for unknown values.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("off") => Self::Off,
            _ => MemoryPolicy::DEFAULT.capture,
        }
    }
}

/// Access mode of a workspace layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerMode {
    /// Read-write layer.
    Rw,
    /// Read-only layer.
    Ro,
}

/// One workspace layer: a memory scope id plus its access mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLayer {
    pub scope_id: String,
    pub mode: LayerMode,
}

impl WorkspaceLayer {
    /// Build a layer from a scope id and access mode.
    pub fn new(scope_id: impl Into<String>, mode: LayerMode) -> Self {
        Self {
            scope_id: scope_id.into(),
            mode,
        }
    }
}

/// Recall x capture policy for a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryPolicy {
    pub recall: RecallMode,
    pub capture: CaptureMode,
}

impl MemoryPolicy {
    /// Recall from every visible layer, capture into the writable layer.
    pub const DEFAULT: Self = Self {
        recall: RecallMode::Visible,
        capture: CaptureMode::Writable,
    };

    /// True when the turn may write memories.
    pub fn captures(&self) -> bool {
        self.capture == CaptureMode::Writable
    }

    /// Scope the turn writes to, or `None` when capture is off.
    pub fn capture_scope(&self, layers: &[WorkspaceLayer], fallback: &str) -> Option<String> {
        self.captures()
            .then(|| writable_memory_scope(layers, fallback))
    }

    /// Scopes the turn may recall from, in layer order and deduplicated.
    pub fn recall_scopes(&self, layers: &[WorkspaceLayer], writable_scope_id: &str) -> Vec<String> {
        recall_memory_scopes(*self, layers, writable_scope_id)
    }
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// First read-write layer's scope id, or `fallback` when no layer is writable.
pub fn writable_memory_scope(layers: &[WorkspaceLayer], fallback: &str) -> String {
    layers
        .iter()
        .find(|l| l.mode == LayerMode::Rw)
        .map(|l| l.scope_id.clone())
        .unwrap_or_else(|| fallback.to_string())
}

/// Scopes a turn may recall from under `policy`.
pub fn recall_memory_scopes(
    policy: MemoryPolicy,
    layers: &[WorkspaceLayer],
    writable_scope_id: &str,
) -> Vec<String> {
    match policy.recall {
        RecallMode::Off => Vec::new(),
        RecallMode::Writable => vec![writable_scope_id.to_string()],
        RecallMode::Visible => {
            let mut out = vec![writable_scope_id.to_string()];
            for layer in layers {
                if !out.contains(&layer.scope_id) {
                    out.push(layer.scope_id.clone());
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers() -> Vec<WorkspaceLayer> {
        vec![
            WorkspaceLayer::new("org", LayerMode::Ro),
            WorkspaceLayer::new("team", LayerMode::Rw),
            WorkspaceLayer::new("shared", LayerMode::Ro),
        ]
    }

    #[test]
    fn default_policy_is_visible_and_writable() {
        let p = MemoryPolicy::default();
        assert_eq!(p.recall, RecallMode::Visible);
        assert_eq!(p.capture, CaptureMode::Writable);
        assert!(p.captures());
    }

    #[test]
    fn parse_falls_back_to_default() {
        assert_eq!(RecallMode::parse(Some("off")), RecallMode::Off);
        assert_eq!(RecallMode::parse(Some("writable")), RecallMode::Writable);
        assert_eq!(RecallMode::parse(Some("visible")), RecallMode::Visible);
        assert_eq!(RecallMode::parse(Some("nonsense")), RecallMode::Visible);
        assert_eq!(RecallMode::parse(None), RecallMode::Visible);
        assert_eq!(CaptureMode::parse(Some("off")), CaptureMode::Off);
        assert_eq!(CaptureMode::parse(Some("writable")), CaptureMode::Writable);
        assert_eq!(CaptureMode::parse(Some("nonsense")), CaptureMode::Writable);
        assert_eq!(CaptureMode::parse(None), CaptureMode::Writable);
    }

    #[test]
    fn writable_scope_prefers_first_rw_layer() {
        assert_eq!(writable_memory_scope(&layers(), "self"), "team");
        let read_only = vec![WorkspaceLayer::new("org", LayerMode::Ro)];
        assert_eq!(writable_memory_scope(&read_only, "self"), "self");
        assert_eq!(writable_memory_scope(&[], "self"), "self");
    }

    #[test]
    fn recall_scopes_follow_the_mode() {
        let ls = layers();
        let off = MemoryPolicy {
            recall: RecallMode::Off,
            capture: CaptureMode::Off,
        };
        assert!(off.recall_scopes(&ls, "team").is_empty());

        let writable = MemoryPolicy {
            recall: RecallMode::Writable,
            capture: CaptureMode::Writable,
        };
        assert_eq!(
            writable.recall_scopes(&ls, "team"),
            vec!["team".to_string()]
        );

        assert_eq!(
            MemoryPolicy::DEFAULT.recall_scopes(&ls, "team"),
            vec!["team".to_string(), "org".to_string(), "shared".to_string()]
        );
    }

    #[test]
    fn recall_scopes_deduplicate_and_lead_with_writable() {
        let ls = vec![
            WorkspaceLayer::new("team", LayerMode::Rw),
            WorkspaceLayer::new("team", LayerMode::Ro),
        ];
        assert_eq!(
            MemoryPolicy::DEFAULT.recall_scopes(&ls, "team"),
            vec!["team".to_string()]
        );
    }

    #[test]
    fn capture_scope_is_none_when_capture_off() {
        let ls = layers();
        let off = MemoryPolicy {
            recall: RecallMode::Visible,
            capture: CaptureMode::Off,
        };
        assert_eq!(off.capture_scope(&ls, "self"), None);
        assert_eq!(
            MemoryPolicy::DEFAULT.capture_scope(&ls, "self"),
            Some("team".to_string())
        );
    }

    #[test]
    fn test_recall_memory_scopes() {
        let ls = layers();

        let off_policy = MemoryPolicy {
            recall: RecallMode::Off,
            capture: CaptureMode::Off,
        };
        assert!(recall_memory_scopes(off_policy, &ls, "team").is_empty());

        let writable_policy = MemoryPolicy {
            recall: RecallMode::Writable,
            capture: CaptureMode::Writable,
        };
        assert_eq!(
            recall_memory_scopes(writable_policy, &ls, "team"),
            vec!["team".to_string()]
        );

        let visible_policy = MemoryPolicy {
            recall: RecallMode::Visible,
            capture: CaptureMode::Writable,
        };
        assert_eq!(
            recall_memory_scopes(visible_policy, &ls, "team"),
            vec!["team".to_string(), "org".to_string(), "shared".to_string()]
        );

        let ls_dup = vec![
            WorkspaceLayer::new("team", LayerMode::Rw),
            WorkspaceLayer::new("org", LayerMode::Ro),
            WorkspaceLayer::new("team", LayerMode::Ro),
        ];
        assert_eq!(
            recall_memory_scopes(visible_policy, &ls_dup, "team"),
            vec!["team".to_string(), "org".to_string()]
        );
    }
}

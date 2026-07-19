//! Lifecycle hooks: real async dispatch with parking_lot (grok pattern).

use crate::agent::{Event, ToolCall, ToolResult};
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum HookEvent {
    BeforePrompt { text: String },
    BeforeTool { tool: ToolCall },
    AfterTool { tool: ToolCall, result: ToolResult },
    AfterTurn { turn: usize },
    AgentEvent(Event),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    Deny { reason: String },
    ModifyArgs { arguments: String },
}

pub type HookFn = Arc<dyn Fn(&HookEvent) -> HookDecision + Send + Sync>;

pub struct HookRegistry {
    hooks: Mutex<Vec<HookFn>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            hooks: Mutex::new(Vec::new()),
        }
    }

    pub fn add(&self, hook: impl Fn(&HookEvent) -> HookDecision + Send + Sync + 'static) {
        self.hooks.lock().push(Arc::new(hook));
    }

    /// Fire all hooks; ignore decisions (notify-only events).
    pub fn run_notify(&self, event: &HookEvent) {
        let hooks = self.hooks.lock().clone();
        for h in &hooks {
            let _ = h(event);
        }
    }

    /// Aggregate hook decisions. For `BeforeTool`: first Deny wins; ModifyArgs chain.
    pub fn run(&self, event: &HookEvent) -> HookDecision {
        let hooks = self.hooks.lock().clone();
        let mut modified: Option<String> = None;
        for h in &hooks {
            match h(event) {
                HookDecision::Deny { reason } => {
                    return HookDecision::Deny { reason };
                }
                HookDecision::ModifyArgs { arguments } => {
                    modified = Some(arguments);
                }
                HookDecision::Allow => {}
            }
        }
        match modified {
            Some(arguments) => HookDecision::ModifyArgs { arguments },
            None => HookDecision::Allow,
        }
    }

    /// Gate a tool call: apply chained ModifyArgs; Err on Deny.
    pub fn run_before_tool(&self, tool: &ToolCall) -> Result<ToolCall, String> {
        let hooks = self.hooks.lock().clone();
        let mut call = tool.clone();
        for h in &hooks {
            match h(&HookEvent::BeforeTool {
                tool: call.clone(),
            }) {
                HookDecision::Deny { reason } => return Err(reason),
                HookDecision::ModifyArgs { arguments } => {
                    call.arguments = arguments;
                }
                HookDecision::Allow => {}
            }
        }
        Ok(call)
    }

    pub fn count(&self) -> usize {
        self.hooks.lock().len()
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn hooks_fire() {
        let registry = HookRegistry::new();
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        registry.add(move |_| {
            count2.fetch_add(1, Ordering::SeqCst);
            HookDecision::Allow
        });
        registry.run_notify(&HookEvent::BeforePrompt {
            text: "test".into(),
        });
        registry.run_notify(&HookEvent::AfterTurn { turn: 0 });
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn before_tool_deny_wins() {
        let registry = HookRegistry::new();
        registry.add(|_| HookDecision::Allow);
        registry.add(|_| HookDecision::Deny {
            reason: "blocked".into(),
        });
        registry.add(|_| HookDecision::ModifyArgs {
            arguments: r#"{"x":1}"#.into(),
        });
        let call = ToolCall {
            id: "1".into(),
            name: "write".into(),
            arguments: "{}".into(),
        };
        let err = registry.run_before_tool(&call).unwrap_err();
        assert_eq!(err, "blocked");
    }

    #[test]
    fn before_tool_chains_modify_args() {
        let registry = HookRegistry::new();
        registry.add(|_| HookDecision::ModifyArgs {
            arguments: r#"{"a":1}"#.into(),
        });
        registry.add(|ev| match ev {
            HookEvent::BeforeTool { tool } => {
                assert_eq!(tool.arguments, r#"{"a":1}"#);
                HookDecision::ModifyArgs {
                    arguments: r#"{"a":2}"#.into(),
                }
            }
            _ => HookDecision::Allow,
        });
        let call = ToolCall {
            id: "1".into(),
            name: "write".into(),
            arguments: "{}".into(),
        };
        let out = registry.run_before_tool(&call).unwrap();
        assert_eq!(out.arguments, r#"{"a":2}"#);
    }
}

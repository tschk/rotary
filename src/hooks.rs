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

pub type HookFn = Arc<dyn Fn(&HookEvent) + Send + Sync>;

pub struct HookRegistry {
    hooks: Mutex<Vec<HookFn>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            hooks: Mutex::new(Vec::new()),
        }
    }

    pub fn add(&self, hook: impl Fn(&HookEvent) + Send + Sync + 'static) {
        self.hooks.lock().push(Arc::new(hook));
    }

    pub fn run(&self, event: &HookEvent) {
        let hooks = self.hooks.lock().clone();
        for h in &hooks {
            h(event);
        }
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
        });
        registry.run(&HookEvent::BeforePrompt {
            text: "test".into(),
        });
        registry.run(&HookEvent::AfterTurn { turn: 0 });
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}

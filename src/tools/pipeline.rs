//! Registry marker for the Agent-owned declarative tool pipeline.

use crate::agent::{ToolContext, ToolFuture, ToolResult};
use std::sync::Arc;

/// The Agent replaces this marker after the outer call has passed normal
/// hooks, scope, policy, approval, and sandbox setup. Registry-only execution
/// fails closed so it cannot bypass those gates.
pub fn exec_tool_pipeline(_ctx: Arc<ToolContext>, _args: String) -> ToolFuture {
    Box::pin(async {
        ToolResult::err(
            "tool_pipeline",
            "tool_pipeline requires Agent loop execution",
        )
    })
}

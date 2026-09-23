//! Bounded declarative registered-tool pipelines. This deliberately accepts a
//! small JSON plan, not source code or an embedded language runtime.

use super::{normalize_tool_name, Agent, ToolCall, ToolContext, ToolEffect, ToolResult};
use crate::guardrails::schedule_tool_calls;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_STEPS: usize = 12;
const MAX_CONCURRENCY: usize = 4;
const MAX_PLAN_BYTES: usize = 16 * 1024;
const MAX_EXPANDED_ARGUMENT_BYTES: usize = 8 * 1024;
const MAX_REFERENCE_BYTES: usize = 4 * 1024;
const MAX_RETURN_BYTES: usize = 4 * 1024;
const MAX_SELECTED_RESULT_BYTES: usize = 1024;
const PIPELINE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelinePlan {
    steps: Vec<PipelineStep>,
    output: PipelineOutput,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineStep {
    id: String,
    tool: String,
    arguments: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineOutput {
    select: Vec<PipelineSelection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineSelection {
    step: String,
    #[serde(default = "default_selected_result_bytes")]
    max_bytes: usize,
}

fn default_selected_result_bytes() -> usize {
    512
}

impl Agent {
    pub(super) async fn execute_tool_pipeline(
        &self,
        call: &ToolCall,
        ctx: &Arc<ToolContext>,
    ) -> ToolResult {
        let plan = match parse_plan(&call.arguments) {
            Ok(plan) => plan,
            Err(error) => return ToolResult::err(&call.id, error),
        };
        let dependencies = match validate_plan(&plan) {
            Ok(dependencies) => dependencies,
            Err(error) => return ToolResult::err(&call.id, error),
        };

        let deadline = Instant::now() + PIPELINE_TIMEOUT;
        let mut completed = HashMap::<String, ToolResult>::new();
        let mut pending: HashSet<usize> = (0..plan.steps.len()).collect();

        while !pending.is_empty() {
            if ctx.cancellation.is_canceled() {
                return ToolResult::err(&call.id, "tool pipeline cancelled");
            }
            let ready: Vec<usize> = (0..plan.steps.len())
                .filter(|index| {
                    pending.contains(index)
                        && dependencies[*index]
                            .iter()
                            .all(|dependency| completed.contains_key(dependency))
                })
                .collect();
            if ready.is_empty() {
                return ToolResult::err(&call.id, "tool pipeline has unresolved dependencies");
            }

            let mut calls = Vec::with_capacity(ready.len());
            for index in &ready {
                let step = &plan.steps[*index];
                let arguments = match expand_references(&step.arguments, &completed) {
                    Ok(arguments) => arguments,
                    Err(error) => return ToolResult::err(&call.id, error),
                };
                let arguments = match serde_json::to_string(&arguments) {
                    Ok(arguments) if arguments.len() <= MAX_EXPANDED_ARGUMENT_BYTES => arguments,
                    Ok(_) => {
                        return ToolResult::err(
                            &call.id,
                            format!(
                                "pipeline step {} exceeds {MAX_EXPANDED_ARGUMENT_BYTES} expanded argument bytes",
                                step.id
                            ),
                        )
                    }
                    Err(error) => return ToolResult::err(&call.id, format!("invalid arguments: {error}")),
                };
                calls.push(ToolCall {
                    id: format!("{}:{}", call.id, step.id),
                    name: step.tool.clone(),
                    arguments,
                });
            }

            let classified: Vec<(String, String, ToolEffect)> = calls
                .iter()
                .map(|inner| {
                    let name = normalize_tool_name(&inner.name).to_string();
                    (
                        name.clone(),
                        inner.arguments.clone(),
                        self.tools.effect_of(&name),
                    )
                })
                .collect();
            for effect_batch in schedule_tool_calls(&classified) {
                for chunk in effect_batch.chunks(MAX_CONCURRENCY) {
                    let batch: Vec<ToolCall> =
                        chunk.iter().map(|index| calls[*index].clone()).collect();
                    let remaining = match deadline.checked_duration_since(Instant::now()) {
                        Some(remaining) => remaining,
                        None => {
                            return ToolResult::err(
                                &call.id,
                                "tool pipeline timed out after 30 seconds",
                            )
                        }
                    };
                    let results = match ctx
                        .cancellation
                        .run(tokio::time::timeout(
                            remaining,
                            Box::pin(self.execute_tools_parallel(&batch, ctx)),
                        ))
                        .await
                    {
                        Ok(Ok(results)) => results,
                        Ok(Err(_)) => {
                            return ToolResult::err(
                                &call.id,
                                "tool pipeline timed out after 30 seconds",
                            )
                        }
                        Err(_) => return ToolResult::err(&call.id, "tool pipeline cancelled"),
                    };
                    for (inner, result) in batch.iter().zip(results) {
                        let step_id = inner.id.rsplit(':').next().unwrap_or_default();
                        completed.insert(step_id.to_string(), result);
                    }
                }
            }
            pending.retain(|index| !ready.contains(index));
        }

        match selected_output(&plan.output, &completed) {
            Ok(output) => ToolResult::ok(&call.id, output),
            Err(error) => ToolResult::err(&call.id, error),
        }
    }
}

fn parse_plan(arguments: &str) -> Result<PipelinePlan, String> {
    if arguments.len() > MAX_PLAN_BYTES {
        return Err(format!("tool pipeline plan exceeds {MAX_PLAN_BYTES} bytes"));
    }
    serde_json::from_str(arguments).map_err(|error| format!("invalid tool pipeline plan: {error}"))
}

fn validate_plan(plan: &PipelinePlan) -> Result<Vec<Vec<String>>, String> {
    if plan.steps.is_empty() || plan.steps.len() > MAX_STEPS {
        return Err(format!("tool pipeline requires 1 to {MAX_STEPS} steps"));
    }
    if plan.output.select.is_empty() || plan.output.select.len() > MAX_STEPS {
        return Err("tool pipeline output must explicitly select 1 to 12 step results".to_string());
    }

    let mut seen = HashSet::new();
    let mut dependencies = Vec::with_capacity(plan.steps.len());
    for step in &plan.steps {
        if !valid_step_id(&step.id) || !seen.insert(step.id.clone()) {
            return Err(format!(
                "invalid or duplicate pipeline step id: {}",
                step.id
            ));
        }
        if step.tool.is_empty() || step.tool.len() > 128 {
            return Err(format!("invalid pipeline tool name for step {}", step.id));
        }
        if normalize_tool_name(&step.tool) == "tool_pipeline" {
            return Err("tool_pipeline cannot call itself".to_string());
        }
        let refs = references_in(&step.arguments)?;
        for reference in &refs {
            if !seen.contains(reference) {
                return Err(format!(
                    "step {} references {} but references must name an earlier step",
                    step.id, reference
                ));
            }
        }
        // A reference is necessarily to an earlier step because `seen` is
        // populated in declaration order; retain it as the dependency set.
        dependencies.push(refs);
    }
    for selection in &plan.output.select {
        if !seen.contains(&selection.step) {
            return Err(format!(
                "output selects unknown pipeline step: {}",
                selection.step
            ));
        }
        if selection.max_bytes == 0 || selection.max_bytes > MAX_SELECTED_RESULT_BYTES {
            return Err(format!(
                "output selection {} must use 1 to {MAX_SELECTED_RESULT_BYTES} max_bytes",
                selection.step
            ));
        }
    }
    Ok(dependencies)
}

fn valid_step_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn references_in(value: &Value) -> Result<Vec<String>, String> {
    let mut references = Vec::new();
    collect_references(value, &mut references)?;
    Ok(references)
}

fn collect_references(value: &Value, references: &mut Vec<String>) -> Result<(), String> {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_references(value, references)?;
            }
        }
        Value::Object(values) if values.contains_key("$ref") => {
            if values.len() != 1 {
                return Err("a pipeline reference object may contain only $ref".to_string());
            }
            let reference = values
                .get("$ref")
                .and_then(Value::as_str)
                .filter(|reference| valid_step_id(reference))
                .ok_or_else(|| "pipeline $ref must name a valid step id".to_string())?;
            references.push(reference.to_string());
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_references(value, references)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn expand_references(
    value: &Value,
    results: &HashMap<String, ToolResult>,
) -> Result<Value, String> {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|value| expand_references(value, results))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) if values.contains_key("$ref") => {
            let reference = values
                .get("$ref")
                .and_then(Value::as_str)
                .ok_or_else(|| "pipeline $ref must be a string".to_string())?;
            let result = results
                .get(reference)
                .ok_or_else(|| format!("pipeline reference result missing: {reference}"))?;
            Ok(Value::String(truncate_utf8(
                &result.content,
                MAX_REFERENCE_BYTES,
            )))
        }
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), expand_references(value, results)?)))
            .collect::<Result<serde_json::Map<_, _>, String>>()
            .map(Value::Object),
        value => Ok(value.clone()),
    }
}

fn selected_output(
    output: &PipelineOutput,
    results: &HashMap<String, ToolResult>,
) -> Result<String, String> {
    let mut selected = Vec::with_capacity(output.select.len());
    for selection in &output.select {
        let result = results
            .get(&selection.step)
            .ok_or_else(|| format!("pipeline output result missing: {}", selection.step))?;
        let content = truncate_to_fit(
            &selected,
            &selection.step,
            !result.is_error,
            &result.content,
            selection.max_bytes,
        )?;
        selected.push(serde_json::json!({
            "step": selection.step,
            "ok": !result.is_error,
            "content": content,
        }));
    }
    serde_json::to_string(&serde_json::json!({ "results": selected }))
        .map_err(|error| format!("failed to serialize pipeline output: {error}"))
}

fn truncate_to_fit(
    selected: &[Value],
    step: &str,
    ok: bool,
    content: &str,
    max_bytes: usize,
) -> Result<String, String> {
    let limit = max_bytes.min(MAX_SELECTED_RESULT_BYTES);
    for bytes in (0..=limit).rev() {
        let candidate = truncate_utf8(content, bytes);
        let mut output = selected.to_vec();
        output.push(serde_json::json!({ "step": step, "ok": ok, "content": candidate }));
        let serialized = serde_json::to_string(&serde_json::json!({ "results": output }))
            .map_err(|error| format!("failed to serialize pipeline output: {error}"))?;
        if serialized.len() <= MAX_RETURN_BYTES {
            return Ok(truncate_utf8(content, bytes));
        }
    }
    Err("pipeline output metadata exceeds 4 KiB".to_string())
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ToolDefinition;
    use crate::permissions::{Authorizer, Decision, Policy};
    use crate::tools::register_builtin_tools;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn pipeline_call(plan: Value) -> ToolCall {
        ToolCall {
            id: "pipeline".to_string(),
            name: "tool_pipeline".to_string(),
            arguments: plan.to_string(),
        }
    }

    fn pipeline_plan(steps: Value, select: Value) -> Value {
        serde_json::json!({ "steps": steps, "output": { "select": select } })
    }

    async fn execute(agent: &Agent, plan: Value) -> ToolResult {
        let ctx = Arc::new(agent.tool_context());
        agent
            .execute_single_tool(&pipeline_call(plan), &ctx)
            .await
            .1
    }

    #[tokio::test]
    async fn pipeline_runs_registered_calls_and_expands_prior_result() {
        let registry = crate::agent::ToolRegistry::new();
        register_builtin_tools(&registry);
        registry.register(
            ToolDefinition::new_boxed(
                "first",
                "first",
                "{}",
                Box::new(|_ctx, _args| Box::pin(async { ToolResult::ok("first", "alpha") })),
            )
            .with_effect(ToolEffect::Read),
        );
        registry.register(
            ToolDefinition::new_boxed(
                "second",
                "second",
                "{}",
                Box::new(|_ctx, args| Box::pin(async move { ToolResult::ok("second", args) })),
            )
            .with_effect(ToolEffect::Read),
        );
        let mut agent = Agent::new();
        agent.set_tools(registry);
        agent.set_policy(Policy::full_access());

        let result = execute(
            &agent,
            pipeline_plan(
                serde_json::json!([
                    { "id": "first", "tool": "first", "arguments": {} },
                    { "id": "second", "tool": "second", "arguments": { "input": { "$ref": "first" } } }
                ]),
                serde_json::json!([{ "step": "second", "max_bytes": 128 }]),
            ),
        )
        .await;

        assert!(!result.is_error, "{}", result.content);
        let output: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(output["results"][0]["content"], r#"{"input":"alpha"}"#);
    }

    struct DenyInner;

    impl Authorizer for DenyInner {
        fn authorize(
            &self,
            _policy: &Policy,
            tool_name: &str,
            _arguments: &str,
            _approver: Option<&dyn crate::permissions::Approver>,
            _workspace_root: Option<&std::path::Path>,
        ) -> Decision {
            if tool_name == "blocked" {
                Decision::Deny
            } else {
                Decision::Allow
            }
        }
    }

    #[tokio::test]
    async fn pipeline_denied_inner_call_never_reaches_executor() {
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = crate::agent::ToolRegistry::new();
        register_builtin_tools(&registry);
        let calls_for_tool = Arc::clone(&calls);
        registry.register(
            ToolDefinition::new_boxed(
                "blocked",
                "blocked",
                "{}",
                Box::new(move |_ctx, _args| {
                    let calls = Arc::clone(&calls_for_tool);
                    Box::pin(async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        ToolResult::ok("blocked", "unexpected")
                    })
                }),
            )
            .with_effect(ToolEffect::Read),
        );
        let mut agent = Agent::new();
        agent.set_tools(registry);
        agent.set_policy(Policy::full_access());
        agent.set_authorizer(Arc::new(DenyInner));

        let result = execute(
            &agent,
            pipeline_plan(
                serde_json::json!([{ "id": "denied", "tool": "blocked", "arguments": {} }]),
                serde_json::json!([{ "step": "denied" }]),
            ),
        )
        .await;

        assert!(
            !result.is_error,
            "pipeline should report the selected inner error"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let output: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(output["results"][0]["ok"], false);
        assert_eq!(output["results"][0]["content"], "denied by policy");
    }

    #[tokio::test]
    async fn pipeline_rejects_recursion() {
        let registry = crate::agent::ToolRegistry::new();
        register_builtin_tools(&registry);
        let mut agent = Agent::new();
        agent.set_tools(registry);
        agent.set_policy(Policy::full_access());

        let result = execute(
            &agent,
            pipeline_plan(
                serde_json::json!([{ "id": "again", "tool": "tool_pipeline", "arguments": {} }]),
                serde_json::json!([{ "step": "again" }]),
            ),
        )
        .await;

        assert!(result.is_error);
        assert_eq!(result.content, "tool_pipeline cannot call itself");
    }

    #[tokio::test]
    async fn pipeline_bounds_selected_output() {
        let registry = crate::agent::ToolRegistry::new();
        register_builtin_tools(&registry);
        registry.register(
            ToolDefinition::new_boxed(
                "large",
                "large",
                "{}",
                Box::new(|_ctx, _args| {
                    Box::pin(async { ToolResult::ok("large", "x".repeat(3_000)) })
                }),
            )
            .with_effect(ToolEffect::Read),
        );
        let mut agent = Agent::new();
        agent.set_tools(registry);
        agent.set_policy(Policy::full_access());

        let result = execute(
            &agent,
            pipeline_plan(
                serde_json::json!([{ "id": "large", "tool": "large", "arguments": {} }]),
                serde_json::json!([{ "step": "large", "max_bytes": 32 }]),
            ),
        )
        .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.len() <= MAX_RETURN_BYTES);
        let output: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(output["results"][0]["content"].as_str().unwrap().len(), 35);
    }

    fn delayed_read(name: &str, calls: Arc<AtomicUsize>) -> ToolDefinition {
        ToolDefinition::new_boxed(
            name,
            "delayed read",
            "{}",
            Box::new(move |_ctx, _args| {
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    ToolResult::ok("delayed", "done")
                })
            }),
        )
        .with_effect(ToolEffect::Read)
    }

    #[tokio::test]
    async fn pipeline_runs_independent_read_steps_in_parallel() {
        let calls = Arc::new(AtomicUsize::new(0));
        let registry = crate::agent::ToolRegistry::new();
        register_builtin_tools(&registry);
        registry.register(delayed_read("left", Arc::clone(&calls)));
        registry.register(delayed_read("right", Arc::clone(&calls)));
        let mut agent = Agent::new();
        agent.set_tools(registry);
        agent.set_policy(Policy::full_access());

        let start = Instant::now();
        let result = execute(
            &agent,
            pipeline_plan(
                serde_json::json!([
                    { "id": "left", "tool": "left", "arguments": {} },
                    { "id": "right", "tool": "right", "arguments": {} }
                ]),
                serde_json::json!([{ "step": "left" }, { "step": "right" }]),
            ),
        )
        .await;

        assert!(!result.is_error, "{}", result.content);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            start.elapsed() < Duration::from_millis(140),
            "independent reads were not parallel: {:?}",
            start.elapsed()
        );
    }
}

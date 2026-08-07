#![cfg(unix)]

use rx4::{
    AutoresearchBudget, AutoresearchController, AutoresearchControllerConfig, AutoresearchEvent,
    CompletionReason, ExperimentHypothesis, HypothesisOutcome, IterationStatus,
};
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|error| panic!("git {:?} failed to start: {error}", args));
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn repository(initial_state: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet"]);
    git(dir.path(), &["config", "user.name", "test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(dir.path().join("state"), initial_state).unwrap();
    std::fs::write(dir.path().join(".gitignore"), ".metric-count\n").unwrap();
    git(dir.path(), &["add", "state", ".gitignore"]);
    git(dir.path(), &["commit", "--quiet", "-m", "initial"]);
    dir
}

fn config(measure: &str, checks: &str) -> AutoresearchControllerConfig {
    AutoresearchControllerConfig::new("test", "score", measure, checks)
}

#[tokio::test]
async fn rejected_candidate_is_rolled_back_and_logged() {
    let repo = repository("base");
    let mut controller = AutoresearchController::new(
        repo.path(),
        config(
            "if grep -q worse state; then printf 'METRIC score=11\\n'; else printf 'METRIC score=10\\n'; fi",
            "test -f state",
        ),
    )
    .await
    .unwrap();
    controller.establish_baseline().await.unwrap();

    let result = controller
        .run_iteration(
            ExperimentHypothesis::new("worse", "try a slower change"),
            |workspace| async move {
                std::fs::write(workspace.path().join("state"), "worse").unwrap();
                Ok(HypothesisOutcome::default())
            },
        )
        .await
        .unwrap();

    assert_eq!(result.status, IterationStatus::Rejected);
    assert_eq!(
        std::fs::read_to_string(controller.workspace_path().join("state")).unwrap(),
        "base"
    );
    assert!(controller
        .events()
        .iter()
        .any(|event| matches!(event, AutoresearchEvent::Rejected { .. })));
    assert_eq!(
        std::fs::read_to_string(repo.path().join("state")).unwrap(),
        "base"
    );
    controller.close().await.unwrap();
}

#[tokio::test]
async fn noisy_measurements_use_the_median_before_accepting() {
    let repo = repository("base");
    let mut experiment_config = config(
        "count=$(cat .metric-count 2>/dev/null || echo 0); printf '%s\\n' $((count + 1)) > .metric-count; if grep -q candidate state; then case $((count % 3)) in 0) printf 'METRIC score=8\\n';; 1) printf 'METRIC score=50\\n';; *) printf 'METRIC score=8\\n';; esac; else case $((count % 3)) in 0) printf 'METRIC score=10\\n';; 1) printf 'METRIC score=100\\n';; *) printf 'METRIC score=10\\n';; esac; fi",
        "test -f state",
    );
    experiment_config.measurement_runs = 3;
    let mut controller = AutoresearchController::new(repo.path(), experiment_config)
        .await
        .unwrap();
    // The command creates an ignored counter so each three-run aggregate is
    // [10, 100, 10] for baseline and [8, 50, 8] for the candidate.
    controller.establish_baseline().await.unwrap();
    let result = controller
        .run_iteration(
            ExperimentHypothesis::new("candidate", "try the faster change"),
            |workspace| async move {
                std::fs::write(workspace.path().join("state"), "candidate").unwrap();
                Ok(HypothesisOutcome::default())
            },
        )
        .await
        .unwrap();

    assert_eq!(result.status, IterationStatus::Accepted);
    assert_eq!(result.metric, Some(8.0));
    assert_eq!(result.samples, vec![8.0, 50.0, 8.0]);
    controller.close().await.unwrap();
}

#[tokio::test]
async fn guard_failure_rejects_even_when_metric_improves() {
    let repo = repository("valid");
    let mut controller = AutoresearchController::new(
        repo.path(),
        config(
            "if test \"$(cat state)\" = invalid; then printf 'METRIC score=1\\n'; else printf 'METRIC score=10\\n'; fi",
            "test \"$(cat state)\" = valid",
        ),
    )
    .await
    .unwrap();
    controller.establish_baseline().await.unwrap();
    let result = controller
        .run_iteration(
            ExperimentHypothesis::new("invalid", "break correctness"),
            |workspace| async move {
                std::fs::write(workspace.path().join("state"), "invalid").unwrap();
                Ok(HypothesisOutcome::default())
            },
        )
        .await
        .unwrap();

    assert_eq!(result.status, IterationStatus::Rejected);
    assert_eq!(result.metric, Some(1.0));
    assert_eq!(
        std::fs::read_to_string(controller.workspace_path().join("state")).unwrap(),
        "valid"
    );
    controller.close().await.unwrap();
}

#[tokio::test]
async fn cancellation_kills_measurement_and_completes_cancelled() {
    let repo = repository("base");
    let mut controller = AutoresearchController::new(
        repo.path(),
        config(
            "if test \"$(cat state)\" = base; then printf 'METRIC score=10\\n'; else sleep 5; printf 'METRIC score=1\\n'; fi",
            "test -f state",
        ),
    )
    .await
    .unwrap();
    controller.establish_baseline().await.unwrap();
    let cancellation = controller.cancellation_handle();
    let cancel_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancellation.cancel();
    });
    let result = controller
        .run_iteration(
            ExperimentHypothesis::new("slow", "run a cancellable experiment"),
            |workspace| async move {
                std::fs::write(workspace.path().join("state"), "candidate").unwrap();
                Ok(HypothesisOutcome::default())
            },
        )
        .await
        .unwrap();
    cancel_task.await.unwrap();

    assert_eq!(result.status, IterationStatus::Failed);
    assert!(matches!(
        controller.completion().map(|completion| &completion.reason),
        Some(CompletionReason::Cancelled)
    ));
    assert_eq!(
        std::fs::read_to_string(controller.workspace_path().join("state")).unwrap(),
        "base"
    );
    controller.close().await.unwrap();
}

#[tokio::test]
async fn cost_and_iteration_budgets_are_host_visible() {
    let repo = repository("base");
    let mut experiment_config = config("printf 'METRIC score=5\\n'", "test -f state");
    experiment_config.budget = AutoresearchBudget {
        max_iterations: Some(1),
        max_duration_seconds: None,
        max_cost_usd: Some(1.0),
        max_disk_bytes: None,
    };
    let mut controller = AutoresearchController::new(repo.path(), experiment_config)
        .await
        .unwrap();
    controller.establish_baseline().await.unwrap();

    let result = controller
        .run_iteration(
            ExperimentHypothesis::new("costly", "spend too much"),
            |workspace| async move {
                std::fs::write(workspace.path().join("state"), "candidate").unwrap();
                Ok(HypothesisOutcome { cost_usd: 2.0 })
            },
        )
        .await
        .unwrap();
    assert_eq!(result.status, IterationStatus::Failed);
    assert!(matches!(
        controller.completion().map(|completion| &completion.reason),
        Some(CompletionReason::CostBudget)
    ));
    assert!(controller
        .events()
        .iter()
        .any(|event| matches!(event, AutoresearchEvent::Completed { .. })));
    controller.close().await.unwrap();
}

#[tokio::test]
async fn duration_budget_rolls_back_before_the_next_measurement() {
    let repo = repository("base");
    let mut experiment_config = config("printf 'METRIC score=1\\n'", "test -f state");
    experiment_config.budget.max_duration_seconds = Some(1);
    let mut controller = AutoresearchController::new(repo.path(), experiment_config)
        .await
        .unwrap();
    controller.establish_baseline().await.unwrap();

    let result = controller
        .run_iteration(
            ExperimentHypothesis::new("slow", "consume the wall-clock budget"),
            |workspace| async move {
                std::fs::write(workspace.path().join("state"), "candidate").unwrap();
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok(HypothesisOutcome::default())
            },
        )
        .await
        .unwrap();

    assert_eq!(result.status, IterationStatus::Failed);
    assert!(matches!(
        controller.completion().map(|completion| &completion.reason),
        Some(CompletionReason::DurationBudget)
    ));
    assert_eq!(
        std::fs::read_to_string(controller.workspace_path().join("state")).unwrap(),
        "base"
    );
    controller.close().await.unwrap();
}

#[tokio::test]
async fn final_patch_requires_explicit_acceptance() {
    let repo = repository("base");
    let mut experiment_config = config(
        "printf 'measurement artifact\\n' > measurement-artifact; if test \"$(cat state)\" = candidate; then printf 'METRIC score=1\\n'; else printf 'METRIC score=10\\n'; fi",
        "test -f state",
    );
    experiment_config.budget.max_iterations = Some(1);
    let mut controller = AutoresearchController::new(repo.path(), experiment_config)
        .await
        .unwrap();
    controller.establish_baseline().await.unwrap();
    controller
        .run_iteration(
            ExperimentHypothesis::new("candidate", "change state"),
            |workspace| async move {
                std::fs::write(workspace.path().join("state"), "candidate").unwrap();
                Ok(HypothesisOutcome::default())
            },
        )
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(repo.path().join("state")).unwrap(),
        "base"
    );
    let patch = controller.final_patch().await.unwrap();
    assert!(patch.changed_files.contains(&"state".to_string()));
    assert!(!patch
        .changed_files
        .contains(&"measurement-artifact".to_string()));
    assert!(!controller
        .workspace_path()
        .join("measurement-artifact")
        .exists());
    controller.accept_final_patch().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("state")).unwrap(),
        "candidate"
    );
    controller.close().await.unwrap();
}

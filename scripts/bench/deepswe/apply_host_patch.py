"""Pier custom agent: apply a host-produced patch, commit, then exit.

Used after host-side adapters (tk/codex/pi/omp/fx). The DeepSWE collect hook
is `git diff <base> HEAD`, so the applied patch must be committed.
"""
from __future__ import annotations

from pathlib import Path

from pier.agents.base import BaseAgent
from pier.environments.base import BaseEnvironment
from pier.models.agent.context import AgentContext


class ApplyHostPatchAgent(BaseAgent):
    SUPPORTS_ATIF = False

    def __init__(self, *args, patch_file: str | None = None, **kwargs):
        self.patch_file = patch_file
        super().__init__(*args, **kwargs)

    @staticmethod
    def name() -> str:
        return "apply-host-patch"

    def version(self) -> str:
        return "1.0.0"

    async def setup(self, environment: BaseEnvironment) -> None:
        return None

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        if not self.patch_file:
            return
        src = Path(self.patch_file)
        if not src.exists() or src.stat().st_size == 0:
            return
        dest = "/tmp/host.patch"
        await environment.upload_file(str(src), dest)
        await environment.exec(
            "cd /app && git config --global --add safe.directory /app && "
            f"(git apply --binary {dest} || git apply -p1 --binary {dest} || patch -p1 < {dest}) && "
            "git add -A && "
            "git -c user.email=bench@rotary -c user.name=rotary-bench "
            "commit -m 'host adapter patch' || true"
        )

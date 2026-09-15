"""Harbor BaseAgent that runs host-built tk inside the trial container.

Coding Plan only: glm-5.3-flash via ZAI_API_KEY. Not OpenCode Go.
"""

from __future__ import annotations

import os
import shlex
from pathlib import Path

from harbor.agents.base import BaseAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext


class TkHarborAgent(BaseAgent):
    @staticmethod
    def name() -> str:
        return "tk"

    def version(self) -> str:
        return "0.1.0"

    def _tk_bin(self) -> Path:
        override = os.environ.get("TK_BIN")
        if override:
            return Path(override)
        root = os.environ.get("TELEKINESIS_ROOT", "/home/user/workspace/repo")
        return Path(root) / "ui/tui/target/release/tk"

    async def setup(self, environment: BaseEnvironment) -> None:
        tk = self._tk_bin()
        if not tk.is_file():
            raise FileNotFoundError(f"tk binary missing: {tk}")
        await environment.upload_file(tk, "/usr/local/bin/tk")
        await environment.exec("chmod +x /usr/local/bin/tk", user="root")

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        key = os.environ.get("ZAI_API_KEY", "")
        if not key:
            raise RuntimeError("ZAI_API_KEY is not set")
        prompt = instruction.strip() or "complete the task"
        cmd = (
            "tk exec --provider zai --model glm-5.3-flash --effort low "
            + shlex.quote(prompt)
        )
        result = await environment.exec(
            cmd,
            env={"ZAI_API_KEY": key},
            timeout_sec=int(os.environ.get("BENCH_AGENT_TIMEOUT", "900")),
        )
        log = self.logs_dir / "tk-harbor.log"
        log.write_text(
            f"exit={result.return_code}\nstdout:\n{result.stdout or ''}\n"
            f"stderr:\n{result.stderr or ''}\n"
        )
        if result.return_code != 0:
            raise RuntimeError(
                f"tk exec failed ({result.return_code}): {result.stderr or result.stdout}"
            )

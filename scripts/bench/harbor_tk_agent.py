"""Harbor BaseAgent that runs host-built tk inside the trial container.

Coding Plan only: glm-5.3-flash via ZAI_API_KEY. Not OpenCode Go.

Uses a musl-static tk when present so old Harbor images (glibc < 2.32)
do not crash. Does not append task-specific hints (no pmars). Exec timeout
stays under typical Harbor agent timeouts so the trial can still verify.
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
        return "0.3.0"

    def _tk_bin(self) -> Path:
        for candidate in (
            os.environ.get("TK_BIN_MUSL"),
            os.environ.get("TK_BIN"),
        ):
            if candidate:
                path = Path(candidate)
                if path.is_file():
                    return path
        root = Path(os.environ.get("TELEKINESIS_ROOT", "/home/user/workspace/repo"))
        musl = root / "ui/tui/target/x86_64-unknown-linux-musl/release/tk"
        if musl.is_file():
            return musl
        gnu = root / "ui/tui/target/release/tk"
        if gnu.is_file():
            return gnu
        raise FileNotFoundError("tk binary missing (set TK_BIN_MUSL or TK_BIN)")

    def _exec_timeout_sec(self) -> int:
        # Harbor wraps agent.run() in wait_for(task.agent.timeout_sec), often 900s.
        # Keep docker exec shorter so we return and Harbor can still verify.
        requested = int(os.environ.get("BENCH_AGENT_TIMEOUT", "840"))
        return max(60, min(requested, 840))

    async def setup(self, environment: BaseEnvironment) -> None:
        tk = self._tk_bin()
        await environment.upload_file(tk, "/usr/local/bin/tk")
        chmod = await environment.exec("chmod +x /usr/local/bin/tk", user="root")
        if chmod.return_code != 0:
            raise RuntimeError(chmod.stderr or chmod.stdout or "chmod tk failed")
        probe = await environment.exec("tk exec --help >/dev/null")
        if probe.return_code != 0:
            raise RuntimeError(
                f"tk does not run in this image: {probe.stderr or probe.stdout}"
            )

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        key = os.environ.get("ZAI_API_KEY", "")
        if not key:
            raise RuntimeError("ZAI_API_KEY is not set")
        prompt = instruction.strip()
        quoted = shlex.quote(prompt)
        cmd = (
            "mkdir -p /tmp/tk && printf '%s\\n' "
            + quoted
            + " > /tmp/tk/prompt.txt && "
            "tk exec --provider zai --model glm-5.3-flash --effort low "
            "--cwd /app - < /tmp/tk/prompt.txt"
        )
        result = await environment.exec(
            cmd,
            cwd="/app",
            env={
                "ZAI_API_KEY": key,
                "TK_MAX_TURNS": os.environ.get("TK_MAX_TURNS", "120"),
            },
            timeout_sec=self._exec_timeout_sec(),
        )
        log = self.logs_dir / "tk-harbor.log"
        log.write_text(
            f"exit={result.return_code}\nstdout:\n{result.stdout or ''}\n"
            f"stderr:\n{result.stderr or ''}\n"
        )
        # Do not raise on non-zero: Harbor should still run the verifier on
        # whatever the agent left in the workspace.

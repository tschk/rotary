"""Harbor BaseAgent that runs host-built tk inside the trial container.

Coding Plan only: glm-5.3-flash via ZAI_API_KEY. Not OpenCode Go.

Uses a musl-static tk when present so old Harbor images (glibc < 2.32)
do not crash. Does not append task-specific hints (no pmars). Exec timeout
stays under typical Harbor agent timeouts so the trial can still verify.
"""

from __future__ import annotations

import os
import shlex
import tempfile
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
        # Z.ai's GLM-5.3-Flash TB 2.1 number (84.3) used Claude Code with a
        # 6h timeout. Default Harbor task timeout is 900s; set
        # BENCH_AGENT_TIMEOUT / Harbor --timeout-multiplier to match.
        requested = int(os.environ.get("BENCH_AGENT_TIMEOUT", "21600"))
        return max(60, min(requested, 21600))

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
        # Upload the key as a file. Harbor `env=` becomes `docker compose exec -e`
        # and leaks the secret in `ps`.
        with tempfile.NamedTemporaryFile("w", delete=False) as handle:
            handle.write("ZAI_API_KEY=")
            handle.write(key)
            handle.write("\n")
            key_host = Path(handle.name)
        try:
            await environment.exec("mkdir -p /tmp/tk", user="root")
            await environment.upload_file(key_host, "/tmp/tk/zai.env")
        finally:
            key_host.unlink(missing_ok=True)
        chmod = await environment.exec("chmod 600 /tmp/tk/zai.env", user="root")
        if chmod.return_code != 0:
            raise RuntimeError(chmod.stderr or chmod.stdout or "chmod zai.env failed")
        cmd = (
            "mkdir -p /tmp/tk && printf '%s\\n' "
            + quoted
            + " > /tmp/tk/prompt.txt && "
            "set -a && . /tmp/tk/zai.env && set +a && "
            "tk exec --provider zai --model glm-5.3-flash --effort low "
            "--cwd /app - < /tmp/tk/prompt.txt; "
            "status=$?; rm -f /tmp/tk/zai.env; exit $status"
        )
        result = await environment.exec(
            cmd,
            cwd="/app",
            env={
                "TK_MAX_TURNS": os.environ.get("TK_MAX_TURNS", "400"),
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

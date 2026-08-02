"""Typed, shell-free Python access to OpenECS command-line validation."""

from __future__ import annotations

from dataclasses import dataclass
import subprocess
from typing import Sequence


@dataclass(frozen=True, slots=True)
class OpenEcsRun:
    """Captured OpenECS execution result."""

    command: tuple[str, ...]
    returncode: int
    stdout: str
    stderr: str

    def check_returncode(self) -> None:
        """Raise ``CalledProcessError`` when OpenECS rejected the request."""
        if self.returncode:
            raise subprocess.CalledProcessError(
                self.returncode,
                self.command,
                output=self.stdout,
                stderr=self.stderr,
            )


def run_openecs(
    arguments: Sequence[str], *, executable: str = "openecs"
) -> OpenEcsRun:
    """Run OpenECS without a shell and capture its exact result."""
    command = [executable, *arguments]
    if any("\0" in argument for argument in command):
        raise ValueError("OpenECS command arguments may not contain NUL")
    completed = subprocess.run(
        command,
        check=False,
        capture_output=True,
        text=True,
    )
    return OpenEcsRun(
        command=tuple(completed.args),
        returncode=completed.returncode,
        stdout=completed.stdout,
        stderr=completed.stderr,
    )


__all__ = ["OpenEcsRun", "run_openecs"]


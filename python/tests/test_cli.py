from __future__ import annotations

import subprocess
from unittest import mock

from oheeg_val import OpenEcsRun, run_openecs
from oheeg_val.cli import main


def test_run_openecs_passes_arguments_without_a_shell() -> None:
    completed = subprocess.CompletedProcess(
        ["openecs", "verify-corpus", "--corpus-manifest", "corpus.toml"],
        0,
        "valid\n",
        "",
    )
    with mock.patch("oheeg_val.subprocess.run", return_value=completed) as run:
        result = run_openecs(
            ["verify-corpus", "--corpus-manifest", "corpus.toml"],
            executable="openecs",
        )
    run.assert_called_once_with(
        ["openecs", "verify-corpus", "--corpus-manifest", "corpus.toml"],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result == OpenEcsRun(
        command=tuple(completed.args),
        returncode=0,
        stdout="valid\n",
        stderr="",
    )


def test_run_openecs_rejects_nul_arguments() -> None:
    try:
        run_openecs(["grade", "bad\0path"])
    except ValueError as error:
        assert "NUL" in str(error)
    else:
        raise AssertionError("NUL argument accepted")


def test_cli_preserves_openecs_exit_status() -> None:
    completed = subprocess.CompletedProcess(["openecs", "grade"], 4)
    with mock.patch("oheeg_val.cli.subprocess.run", return_value=completed) as run:
        assert main(["grade"]) == 4
    run.assert_called_once_with(["openecs", "grade"], check=False)


def test_cli_reports_missing_openecs(capsys: object) -> None:
    del capsys
    with mock.patch(
        "oheeg_val.cli.subprocess.run", side_effect=FileNotFoundError("openecs")
    ):
        assert main(["--version"]) == 127

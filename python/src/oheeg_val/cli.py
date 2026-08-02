"""Console launcher preserving OpenECS arguments and exit status."""

from __future__ import annotations

import subprocess
import sys
from typing import Sequence


def main(argv: Sequence[str] | None = None) -> int:
    arguments = list(sys.argv[1:] if argv is None else argv)
    if any("\0" in argument for argument in arguments):
        print("oheeg-val: arguments may not contain NUL", file=sys.stderr)
        return 2
    try:
        completed = subprocess.run(["openecs", *arguments], check=False)
    except FileNotFoundError:
        print(
            "oheeg-val: openecs executable not found; install "
            "open-eeg-codec-standard first",
            file=sys.stderr,
        )
        return 127
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())

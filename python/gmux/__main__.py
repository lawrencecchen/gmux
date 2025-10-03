"""Entrypoint for the gmux console script."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

from . import binary_path


def main() -> None:
    binary = binary_path()
    if not binary.exists():
        raise SystemExit("gmux binary not found in package")

    # Ensure the binary is executable even if the archive stripped bits.
    mode = binary.stat().st_mode
    if not mode & 0o111:
        binary.chmod(mode | 0o755)

    result = subprocess.call([str(binary), *sys.argv[1:]])
    raise SystemExit(result)


if __name__ == "__main__":
    main()

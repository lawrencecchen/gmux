"""gmux Python wrapper."""

from __future__ import annotations

from pathlib import Path

__all__ = ["binary_path", "__version__"]

__version__ = "0.1.0"


def binary_path() -> Path:
    """Return the filesystem path to the bundled gmux binary."""
    return Path(__file__).resolve().parent / "bin" / "gmux"

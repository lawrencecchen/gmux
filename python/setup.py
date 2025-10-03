from __future__ import annotations

from setuptools import setup
from setuptools.dist import Distribution

try:
    from wheel.bdist_wheel import bdist_wheel as _bdist_wheel
except ModuleNotFoundError:  # pragma: no cover - wheel is always present during build
    _bdist_wheel = None


class BinaryWheel(_bdist_wheel if _bdist_wheel else object):
    def finalize_options(self):  # type: ignore[override]
        if _bdist_wheel is None:  # pragma: no cover
            return
        super().finalize_options()
        self.python_tag = "py3"
        self.plat_name = "macosx_11_0_arm64"
        self.universal = False


class BinaryDistribution(Distribution):
    def has_ext_modules(self):  # type: ignore[override]
        return True

    def is_pure(self):  # type: ignore[override]
        return False


setup(
    distclass=BinaryDistribution,
    cmdclass={"bdist_wheel": BinaryWheel} if _bdist_wheel else {},
)

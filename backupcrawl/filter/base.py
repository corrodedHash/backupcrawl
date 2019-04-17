from typing import List, Tuple, Callable
from pathlib import Path

FilterResult = Tuple[bool, bool]
FilterType = Callable[[Path], FilterResult]


class IgnoreFilter:
    def __init__(self, ignore_list: List[Path]) -> None:
        self.ignore_list = ignore_list

    def __call__(self, current_file: Path) -> FilterResult:
        if current_file in self.ignore_list:
            return (True, True)
        return (False, False)


def SymlinkFilter() -> FilterType:
    return lambda x: (Path.is_symlink(x), False)


def PermissionFilter() -> FilterType:
    def _internal_filter(current_file: Path) -> FilterResult:
        try:
            (current_file / 'hehehehe').exists()
        except PermissionError:
            return (True, False)
        return (False, False)
    return _internal_filter


def WeirdFiletypeFilter() -> FilterType:
    def _internal_filter(current_file: Path) -> FilterResult:
        if current_file.is_dir() or current_file.is_file:
            return (False, False)
        return (True, False)
    return _internal_filter

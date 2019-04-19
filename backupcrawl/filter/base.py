from typing import List, Tuple, Callable
from pathlib import Path
import enum

class FilterResult(enum.Enum):
    PASS = enum.auto()
    DENY = enum.auto()
    IGNORE = enum.auto()

FilterType = Callable[[Path], FilterResult]

class IgnoreFilter:
    def __init__(self, ignore_list: List[Path]) -> None:
        self.ignore_list = ignore_list

    def __call__(self, current_file: Path) -> FilterResult:
        if current_file in self.ignore_list:
            return FilterResult.IGNORE 
        return FilterResult.PASS


def SymlinkFilter() -> FilterType:
    return lambda x: FilterResult.IGNORE if Path.is_symlink(x) else FilterResult.PASS


def PermissionFilter() -> FilterType:
    def _internal_filter(current_file: Path) -> FilterResult:
        if not current_file.is_dir():
            return FilterResult.PASS
        try:
            (current_file / 'hehehehe').exists()
        except PermissionError:
            return FilterResult.IGNORE
        return FilterResult.PASS 
    return _internal_filter


def WeirdFiletypeFilter() -> FilterType:
    def _internal_filter(current_file: Path) -> FilterResult:
        if current_file.is_dir() or current_file.is_file:
            return FilterResult.PASS 
        return FilterResult.IGNORE 
    return _internal_filter

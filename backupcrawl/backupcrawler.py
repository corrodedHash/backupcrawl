"""Contains BackupCrawler class"""

import logging
from typing import List, Tuple, Optional
from pathlib import Path
import enum
from .filter.base import (WeirdFiletypeFilter, SymlinkFilter, PermissionFilter,
                          IgnoreFilter, FilterType)
from .filter.pacman import PacmanFilter
from .filter.git import GitRootFilter, GitRepo

MODULE_LOGGER = logging.getLogger("backupcrawler")


class FileScanResult(enum.Enum):
    """Whether or not a subtree contains a versioncontrolled directory"""
    NO_VC = enum.auto()
    VC = enum.auto()


def crawl(root: Path,
          filter_chain: List[FilterType]) \
        -> Tuple[bool, List[Path]]:
    """Iterates depth first looking for git repositories"""
    MODULE_LOGGER.debug("Entering %s", root)
    result: List[Path] = []
    split_tree: bool = False

    for current_file in root.iterdir():

        for filefilter in filter_chain:
            breakflag, splitflag = filefilter(current_file)
            if splitflag:
                split_tree = True
            if breakflag:
                break
        else:

            current_result = _git_crawl(current_file, filter_chain)

            if current_result[0]:
                result.extend(current_result[1])
                split_tree = True
            elif current_result[1]:
                result.append(current_file)

    return (split_tree, result)


def arch_scan(root: Path,
              ignore_paths: Optional[List[Path]] = None) -> None:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []

    filter_chain: List[FilterType] = []
    filter_chain.append(IgnoreFilter(ignore_paths))
    filter_chain.append(SymlinkFilter())
    filter_chain.append(WeirdFiletypeFilter())
    filter_chain.append(PermissionFilter())
    git_filter = GitRootFilter()
    pacman_filter = PacmanFilter()
    filter_chain.append(git_filter)
    filter_chain.append(pacman_filter)

    _, paths = crawl(root, filter_chain)

    for path in paths:
        print(f"{path}")
    for name, git_paths in [
            ("Dirty git repositories", git_filter.dirty_repos),
            ("Ahead git repositories", git_filter.unsynced_repos),
            ("Clean git repositories", git_filter.clean_repos),
            ("Changed pacman files", pacman_filter.changed_files),
            ("Clean pacman files", pacman_filter.clean_files)]:
        print(f"{name}:")
        for path in git_paths:
            print(f"\t{path}")

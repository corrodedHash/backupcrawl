"""Contains BackupCrawler class"""

import logging
from typing import List, Tuple, Optional, Union
from pathlib import Path
import enum
from .filter.base import (SymlinkFilter, PermissionFilter,
                          IgnoreFilter, FilterType, FilterResult)
from .filter.switch import Switch, FilterChain
from .filter.pacman import PacmanFilter
from .filter.git import GitRootFilter, GitRepo

MODULE_LOGGER = logging.getLogger("backupcrawler")


class FileScanResult(enum.Enum):
    """Whether or not a subtree contains a versioncontrolled directory"""
    NO_VC = enum.auto()
    VC = enum.auto()


def crawl(root: Path,
          filter_chain: FilterChain) \
        -> Tuple[bool, List[Path]]:
    """Iterates depth first looking for git repositories"""
    MODULE_LOGGER.debug("Entering %s", root)
    result: List[Path] = []
    split_tree: bool = False

    for current_file in root.iterdir():
        current_chain = filter_chain
        drop_file = True
        while True:
            drop_file = True
            for filefilter in current_chain[0]:
                filterresult = filefilter(current_file)
                if filterresult == FilterResult.DENY:
                    split_tree = True
                    break
                if filterresult == FilterResult.IGNORE:
                    break
            else:
                drop_file = False
            if drop_file:
                break
            if current_chain[1]:
                current_chain = current_chain[1].get_branch(current_file)
            else:
                break
        if not drop_file:
            if current_file.is_dir():
                current_result = crawl(current_file, filter_chain)

                if current_result[0]:
                    result.extend(current_result[1])
                    split_tree = True
                elif current_result[1]:
                    result.append(current_file)
            elif current_file.is_file():
                result.append(current_file)

    return (split_tree, result)


def arch_scan(root: Path,
              ignore_paths: Optional[List[Path]] = None) -> None:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []

    pacman_filter = PacmanFilter()
    git_filter = GitRootFilter()

    is_dir_switch = Switch(lambda path: Path.is_dir(path))
    is_dir_switch.true_branch[0].append(git_filter)
    is_dir_switch.false_branch[0].append(lambda x: FilterResult.IGNORE)

    is_file_switch = Switch(lambda path: Path.is_file(path))
    is_file_switch.true_branch[0].append(pacman_filter)
    is_file_switch.false_branch = ([], 
            is_dir_switch)

    filter_chain: FilterChain = [[], is_file_switch] 
    filter_chain[0].append(IgnoreFilter(ignore_paths))
    filter_chain[0].append(SymlinkFilter())
    filter_chain[0].append(PermissionFilter())

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

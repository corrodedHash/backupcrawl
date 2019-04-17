"""Contains BackupCrawler class"""

import logging
from typing import List, Tuple, Optional
from pathlib import Path
import enum
import asyncio
from .filter import base as crawlfilter

MODULE_LOGGER = logging.getLogger("backupcrawler")


class FileScanResult(enum.Enum):
    """Whether or not a subtree contains a versioncontrolled directory"""
    NO_VC = enum.auto()
    VC = enum.auto()


async def _git_crawl(root: Path,
                     filter_chain: List[crawlfilter.FilterType]) \
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

            current_result = await _git_crawl(current_file, filter_chain)

            if current_result[0]:
                result.extend(current_result[1])
                split_tree = True
            elif current_result[1]:
                result.append(current_file)

    return (split_tree, result)


def scan(root: Path,
         ignore_paths: Optional[List[Path]] = None) \
        -> Tuple[List[Path], List[GitRepo], List[PacmanFile]]:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []

    _, paths, repos, pacman_files = asyncio.run(
        _git_crawl(root, ignore_paths=ignore_paths))

    return (paths, repos, pacman_files)

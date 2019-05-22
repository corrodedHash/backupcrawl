"""Contains crawling functions"""

import logging
from typing import List, Tuple, Optional
from pathlib import Path
import asyncio
from .git_check import GitSyncStatus, git_check_root, GitRepo
from .pacman_check import PacmanSyncStatus, PacmanFile, is_pacman_file

MODULE_LOGGER = logging.getLogger("backupcrawl.crawler")


# This could be a dataclass, but pylint doesnt understand right now
class CrawlResult:
    """Result from crawl of a single directory"""
    def __init__(self) -> None:

        self.loose_paths: List[Path] = list()
        self.repo_info: List[GitRepo] = list()
        self.pacman_files: List[PacmanFile] = list()
        self.split_tree: bool = False

    def extend(self, other: 'CrawlResult', current_file: Path) -> None:
        """Extend current object with another crawl result"""
        self.repo_info.extend(other.repo_info)
        self.pacman_files.extend(other.pacman_files)
        if other.split_tree:
            self.loose_paths.extend(other.loose_paths)
            self.split_tree = True
        elif other.loose_paths:
            self.loose_paths.append(current_file)


async def _dir_crawl(root: Path,
                     ignore_paths: List[Path]) \
        -> CrawlResult:
    """Iterates depth first looking for git repositories"""
    MODULE_LOGGER.debug("Entering %s", root)
    result = CrawlResult()

    for current_file in root.iterdir():

        if current_file in ignore_paths:
            continue

        if current_file.is_symlink():
            continue

        if current_file.is_file():
            pac_result = await is_pacman_file(current_file)
            if pac_result.status == PacmanSyncStatus.NOPAC:
                result.loose_paths.append(current_file)
                continue
            result.pacman_files.append(pac_result)
            result.split_tree = True
            continue

        if not current_file.is_dir():
            # If path is not a directory, or a file,
            # it is some socket or pipe. We don't care
            continue

        try:
            (current_file / 'hehehehe').exists()
        except PermissionError:
            print(f"No permissions for {str(current_file)}")
            continue

        git_status = await git_check_root(current_file)
        if git_status.status != GitSyncStatus.NOGIT:
            result.repo_info.append(git_status)
            result.split_tree = True
            continue

        current_result = await _dir_crawl(current_file, ignore_paths)
        result.extend(current_result, current_file)

    return result


def scan(root: Path,
         ignore_paths: Optional[List[Path]] = None) \
        -> Tuple[List[Path], List[GitRepo], List[PacmanFile]]:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []

    crawl_result = asyncio.run(
        _dir_crawl(root, ignore_paths=ignore_paths))

    return (
        crawl_result.loose_paths,
        crawl_result.repo_info,
        crawl_result.pacman_files)

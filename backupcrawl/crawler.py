"""Contains crawling functions"""

import logging
from typing import List, Tuple, Optional
from pathlib import Path
from fnmatch import fnmatch
import asyncio
import os
from .git_check import git_check_root, GitRepo
from .pacman_check import PacmanSyncStatus, PacmanFile, is_pacman_file

MODULE_LOGGER = logging.getLogger("backupcrawl.crawler")


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

    def append_pacman(self, status: PacmanFile) -> None:
        """Append a pacman file to the crawl result"""
        if status.status == PacmanSyncStatus.NOPAC:
            self.loose_paths.append(status.path)
            return
        self.pacman_files.append(status)
        self.split_tree = True


async def _dir_crawl(root: Path,
                     ignore_paths: List[str],
                     semaphore: asyncio.Semaphore,
                     ) \
        -> CrawlResult:
    """Iterates depth first looking for git repositories"""
    MODULE_LOGGER.info("Entering %s", root)
    result = CrawlResult()
    pacman_calls = []
    recursive_calls = []
    git_calls = []

    async with semaphore:
        for current_file in root.iterdir():

            if any(fnmatch(str(current_file), cur_pattern)
                   for cur_pattern in ignore_paths):
                continue

            if current_file.is_symlink():
                continue

            if current_file.is_file():
                pacman_calls.append(
                    asyncio.create_task(
                        is_pacman_file(
                            current_file,
                            semaphore)))
                continue

            if not current_file.is_dir():
                # If path is not a directory, or a file,
                # it is some socket or pipe. We don't care
                continue

            try:
                (current_file / 'hehehehe').exists()
            except PermissionError:
                MODULE_LOGGER.warning(
                    "No permissions for %s", str(current_file))
                continue

            if (current_file / '.git').is_dir():
                git_calls.append(
                    asyncio.create_task(git_check_root(current_file)))
                result.split_tree = True
                continue

            recursive_calls.append(
                (asyncio.create_task(
                    _dir_crawl(
                        current_file,
                        ignore_paths,
                        semaphore,
                    )
                ), current_file))

    for recursive_call, current_file in recursive_calls:
        result.extend(await recursive_call, current_file)

    for pacman_call in pacman_calls:
        result.append_pacman(await pacman_call)

    for git_call in git_calls:
        result.repo_info.append(await git_call)

    return result


async def _scan_entry(root: Path, ignore_paths: List[str]) -> CrawlResult:
    corecount = os.cpu_count() or 1
    MODULE_LOGGER.info("Using %d cores", corecount)
    return await _dir_crawl(
        root,
        ignore_paths=ignore_paths,
        semaphore=asyncio.Semaphore(corecount)
    )


def scan(root: Path,
         ignore_paths: Optional[List[str]] = None) \
        -> Tuple[List[Path], List[GitRepo], List[PacmanFile]]:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []

    asyncio.set_event_loop(asyncio.new_event_loop())
    crawl_result = asyncio.run(
        _scan_entry(root, ignore_paths), debug=True
    )

    return (
        crawl_result.loose_paths,
        crawl_result.repo_info,
        crawl_result.pacman_files)

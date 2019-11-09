"""Contains crawling functions"""

import asyncio
import logging
import multiprocessing
import os
from fnmatch import fnmatch
from pathlib import Path
from typing import List, Optional

from .git_check import GitRepo, git_check_root
from .pacman_check import PacmanFile, PacmanSyncStatus, is_pacman_file

MODULE_LOGGER = logging.getLogger("backupcrawl.crawler")


class CrawlResult:
    """Result from crawl of a single directory"""

    def __init__(self) -> None:

        self.loose_paths: List[Path] = list()
        self.denied_paths: List[Path] = list()
        self.repo_info: List[GitRepo] = list()
        self.pacman_files: List[PacmanFile] = list()
        self.split_tree: bool = False
        self.path: Path

    def extend(self, other: "CrawlResult") -> None:
        """Extend current object with another crawl result"""
        self.repo_info.extend(other.repo_info)
        self.pacman_files.extend(other.pacman_files)
        self.denied_paths.extend(other.denied_paths)
        if other.split_tree:
            self.loose_paths.extend(other.loose_paths)
            self.split_tree = True
        elif other.loose_paths:
            self.loose_paths.append(other.path)

    def append_pacman(self, status: PacmanFile) -> None:
        """Append a pacman file to the crawl result"""
        if status.status == PacmanSyncStatus.NOPAC:
            self.loose_paths.append(status.path)
            return
        self.pacman_files.append(status)
        self.split_tree = True


def _check_file(path: Path) -> PacmanFile:
    return is_pacman_file(path)


def _check_directory(path: Path) -> GitRepo:
    return git_check_root(path)


def _contains_vcs(path: Path) -> bool:
    return (path / ".git").exists()


def _dir_crawl(root: Path, ignore_paths: List[str]) -> CrawlResult:
    """Iterates depth first looking for git repositories"""
    MODULE_LOGGER.debug("Entering %s", root)
    result = CrawlResult()
    result.path = root
    found_files = []
    found_directories = []
    recurse_dirs = []

    for current_path in root.iterdir():
        if any(fnmatch(str(current_path), cur_pattern) for cur_pattern in ignore_paths):
            continue

        if current_path.is_symlink():
            continue

        if current_path.is_file():
            found_files.append(current_path)
            continue

        if not current_path.is_dir():
            # If path is not a directory, or a file,
            # it is some socket or pipe. We don't care
            continue

        if not os.access(current_path, os.R_OK | os.X_OK):
            result.denied_paths.append(current_path)
            continue

        if _contains_vcs(current_path):
            found_directories.append(current_path)
            continue

        recurse_dirs.append(current_path)

    for vcs_file in found_files:
        result.append_pacman(_check_file(vcs_file))

    for vcs_dirs in found_directories:
        result.repo_info.append(_check_directory(vcs_dirs))

    for recurse_dir in recurse_dirs:
        result.extend(_dir_crawl(recurse_dir, ignore_paths))

    return result


def scan(root: Path, ignore_paths: Optional[List[str]] = None) -> CrawlResult:
    """Scan the given path for files that are not backed up"""
    if not ignore_paths:
        ignore_paths = []

    asyncio.set_event_loop(asyncio.new_event_loop())
    crawl_result = _dir_crawl(root, ignore_paths)

    return crawl_result

"""Main file"""
import logging
from pathlib import Path
import argparse

from . import crawler
from .git_check import GitSyncStatus
from .pacman_check import PacmanSyncStatus


def crawl(path: Path) -> None:
    """Crawl given directory"""
    ignore_paths = [
        "/home/lukas/.npm",
        "/home/lukas/.cache",
        "/home/lukas/.mypy_cache",
        "/home/lukas/.aurget_build",
        "/home/lukas/.debug",
        "/home/lukas/.vscode",
        "/home/lukas/.vim/bundle",
        "/home/lukas/.vim/spell",
        "/home/lukas/.*history*",
        "/home/lukas/.mozilla",
        "/home/lukas/.ccache",
        "/home/lukas/.pylint.d",
        "/home/lukas/.nv",
        "/home/lukas/.zcompdump-*",
        "/home/lukas/.vim/plugged",
    ]

    crawl_result = crawler.scan(
        Path(path), ignore_paths=ignore_paths)

    for standard_file in crawl_result.loose_paths:
        print("\t" + str(standard_file))

    print("Permission denied:")
    for denied_path in crawl_result.denied_paths:
        print("\t" + str(denied_path))

    for enum_state, status_string in (
            (GitSyncStatus.DIRTY, "Dirty repositories"),
            (GitSyncStatus.AHEAD, "Unsynced repositories"),
            (GitSyncStatus.CLEAN_SYNCED, "Clean repositories")):
        print(status_string + ":")
        for git_dir in [t.path for t in crawl_result.repo_info
                        if t.status == enum_state]:
            print("\t" + str(git_dir))

    for pacman_status, status_string in (
            (PacmanSyncStatus.CHANGED, "Changed pacman files"),
            (PacmanSyncStatus.CLEAN, "Clean pacman files")):
        print(status_string + ":")
        for pacman_file in [t for t in crawl_result.pacman_files
                            if t.status == pacman_status]:
            print("\t" + str(pacman_file.path) + " " + pacman_file.package)


def main() -> None:
    """Main function"""
    parser = argparse.ArgumentParser(
        description="Search for non-backed up files")
    parser.add_argument('path', type=Path, default=Path('/'))
    parser.add_argument('--verbose', '-v', action='store_true')
    args = parser.parse_args()
    logging.basicConfig(level="WARNING")
    logging.getLogger("backupcrawl").setLevel(
        "DEBUG" if args.verbose else "WARNING")

    crawl(args.path)


if __name__ == "__main__":
    main()

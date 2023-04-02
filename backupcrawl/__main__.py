"""Main file"""
import argparse
import json
import logging
from pathlib import Path
from typing import Any
import typing

from . import crawler
from .sync_status import SyncStatus
from .crawlresult import CrawlResult

MODULE_LOGGER = logging.getLogger("backupcrawl.main")


def _print_crawl_result(crawl_result: CrawlResult, verbose: bool = False) -> None:
    """Prints result of crawl"""
    for standard_file in crawl_result.loose_paths:
        print("\t" + str(standard_file))

    print("Permission denied:")
    for denied_path in crawl_result.denied_paths:
        print("\t" + str(denied_path))

    desired_sync_states: list[tuple[SyncStatus, str]] = [
        (SyncStatus.DIRTY, "Dirty"),
        (SyncStatus.AHEAD, "Unsynced"),
    ]
    if verbose:
        desired_sync_states.append((SyncStatus.CLEAN, "Clean"))

    for backup_type in crawl_result.backups:
        for enum_state, status_string in desired_sync_states:
            print(f"{backup_type} {status_string}:")
            for git_dir in [
                t.path
                for t in crawl_result.backups[backup_type]
                if t.status == enum_state
            ]:
                print("\t" + str(git_dir))


def _parse_rc(path: Path) -> dict[str, Any]:
    """Parses config file"""
    if not path.exists():
        MODULE_LOGGER.warning("rcfile %s does not exist", path)
        return {}
    with open(path, "r", encoding='utf-8') as rcfile:
        options = typing.cast(dict[str, Any], json.load(rcfile))
    return options


def main() -> None:
    """Main function"""
    parser = argparse.ArgumentParser(description="Search for non-backed up files")
    parser.add_argument("path", type=Path, default=Path("/"))
    parser.add_argument("--debug", "-d", action="count", default=0)
    parser.add_argument(
        "--rcfile", type=Path, default=Path.home() / ".config" / "backupcrawlrc.json"
    )
    parser.add_argument("--verbose", "-v", action="count", default=0)
    parser.add_argument("--progress", "-p", action="store_true")
    parser.add_argument("--ignore", "-i", action="append", default=[])
    args = parser.parse_args()

    logging.basicConfig(level="WARNING")
    logging.getLogger("backupcrawl").setLevel(
        "WARNING" if args.debug == 0 else "INFO" if args.debug == 1 else "DEBUG"
    )
    config = _parse_rc(args.rcfile)
    crawl_result = crawler.scan(
        args.path,
        ignore_paths=config.get("ignore_paths", []) + args.ignore,
        progress=args.progress,
    )
    _print_crawl_result(crawl_result, verbose=args.verbose > 0)


if __name__ == "__main__":
    main()

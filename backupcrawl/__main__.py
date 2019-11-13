"""Main file"""
import argparse
import logging
from pathlib import Path

from . import crawler
from .sync_status import SyncStatus


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

    crawl_result = crawler.scan(Path(path), ignore_paths=ignore_paths)

    for standard_file in crawl_result.loose_paths:
        print("\t" + str(standard_file))

    print("Permission denied:")
    for denied_path in crawl_result.denied_paths:
        print("\t" + str(denied_path))

    for backup_type in crawl_result.backups:
        for enum_state, status_string in (
                (SyncStatus.DIRTY, "Dirty"),
                (SyncStatus.AHEAD, "Unsynced"),
                (SyncStatus.CLEAN, "Clean"),
        ):
            print(f"{backup_type} {status_string}:")
            for git_dir in [
                    t.path for t in crawl_result.backups[backup_type] if t.status == enum_state
            ]:
                print("\t" + str(git_dir))

def main() -> None:
    """Main function"""
    parser = argparse.ArgumentParser(description="Search for non-backed up files")
    parser.add_argument("path", type=Path, default=Path("/"))
    parser.add_argument("--verbose", "-v", action="count", default=0)
    args = parser.parse_args()
    logging.basicConfig(level="WARNING")
    logging.getLogger("backupcrawl").setLevel(
        "WARNING" if args.verbose == 0 else "INFO" if args.verbose == 1 else "DEBUG"
    )

    crawl(args.path)


if __name__ == "__main__":
    main()

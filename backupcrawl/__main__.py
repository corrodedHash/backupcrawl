"""Main file"""
import argparse
import json
import logging
import typing
from pathlib import Path
from typing import Any

import rich.console

from backupcrawl.printer import ConsoleResultPrinter, JsonResultPrinter
from backupcrawl.statustracker import TimingStatusTracker
from . import crawler

MODULE_LOGGER = logging.getLogger("backupcrawl.main")


console = rich.console.Console()


def _parse_rc(path: Path) -> dict[str, Any]:
    """Parses config file"""
    if not path.exists():
        MODULE_LOGGER.warning("rcfile %s does not exist", path)
        return {}
    with open(path, "r", encoding="utf-8") as rcfile:
        options = typing.cast(dict[str, Any], json.load(rcfile))
    return options


def main() -> None:
    """Main function"""
    parser = argparse.ArgumentParser(description="Search for non-backed up files")
    parser.add_argument("path", type=Path, default=Path("/"))
    parser.add_argument("--verbose", "-v", action="count", default=0)
    parser.add_argument(
        "--rcfile", type=Path, default=Path.home() / ".config" / "backupcrawlrc.json"
    )
    parser.add_argument(
        "--all",
        "-a",
        action="store_true",
        help="Show all file paths, also backed up ones",
    )
    parser.add_argument("--progress", "-p", action="store_true")
    parser.add_argument("--ignore", "-i", action="append", default=[])
    parser.add_argument(
        "--format", "-f", choices=["json", "console"], default="console"
    )
    args = parser.parse_args()

    logging.basicConfig(level="WARNING")
    logging.getLogger("backupcrawl").setLevel(
        "WARNING" if args.verbose == 0 else "INFO" if args.verbose == 1 else "DEBUG"
    )
    config = _parse_rc(args.rcfile)
    crawl_result = crawler.scan(
        args.path,
        ignore_paths=config.get("ignore_paths", []) + args.ignore,
        status=(TimingStatusTracker(args.path, console) if args.progress else None),
    )
    if args.format == "json":
        JsonResultPrinter().print(crawl_result, show_clean=args.all)
    else:
        if args.format != "console":
            print("Unknown output format")
        ConsoleResultPrinter(console).print(crawl_result, show_clean=args.all)


if __name__ == "__main__":
    main()

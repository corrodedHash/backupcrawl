"""Main file"""
import logging
from pathlib import Path
from . import backupcrawler


def main(path: str) -> None:
    """Main function"""
    backupcrawler.arch_scan(Path(path))


if __name__ == "__main__":
    logging.basicConfig(level="DEBUG")
    main('/home/lukas')
    #main('/etc')
    # walkbf('/home/lukas/Downloads')

#!/usr/bin/env python3
"""Measure an executable on Linux, including startup and cleanup costs."""
import argparse
import json
import os
from pathlib import Path
import resource
import subprocess
import sys
import time


def measure(command):
    page_mib = os.sysconf("SC_PAGE_SIZE") / 1024 / 1024
    clock_ticks = os.sysconf("SC_CLK_TCK")
    samples = []
    start = time.monotonic()
    with subprocess.Popen(command) as process:
        while process.poll() is None:
            try:
                fields = Path(f"/proc/{process.pid}/stat").read_text().rsplit(")", 1)[1].split()
                samples.append({
                    "seconds": round(time.monotonic() - start, 2),
                    "rss_mib": round(int(fields[21]) * page_mib, 2),
                    "cpu_seconds": (int(fields[11]) + int(fields[12])) / clock_ticks,
                })
            except (FileNotFoundError, ProcessLookupError):
                pass
            time.sleep(0.25)
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return {
        "command": command,
        "exit": process.returncode,
        "wall_seconds": round(time.monotonic() - start, 3),
        "user_seconds": usage.ru_utime,
        "system_seconds": usage.ru_stime,
        "max_rss_mib": usage.ru_maxrss / 1024,
        "samples": samples,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not args.command:
        parser.error("provide an executable and its arguments")
    if sys.platform != "linux":
        parser.error("this probe requires Linux /proc")
    result = measure(args.command)
    print(json.dumps(result))
    return result["exit"]


if __name__ == "__main__":
    sys.exit(main())

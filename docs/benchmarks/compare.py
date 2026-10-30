#!/usr/bin/env python3
"""Compare the memory and idle CPU of Irori and Heroic on Linux.

Each app starts in its own session with an empty, throwaway profile, so real
logins and libraries stay out of it, and every helper process can be found by
session id. See docs/BENCHMARKS.md for the method and its limits.
"""
import argparse
import csv
import os
import signal
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

SETTLE_SECONDS = 45
IDLE_SECONDS = 30
TICKS_PER_SECOND = os.sysconf("SC_CLK_TCK")


def stat_fields(pid):
    # The process name can hold spaces and parentheses; fields resume after the last ")".
    return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()


def session_pids(sid):
    pids = []
    for entry in Path("/proc").iterdir():
        if entry.name.isdigit():
            try:
                if int(stat_fields(entry.name)[3]) == sid:
                    pids.append(int(entry.name))
            except (OSError, ValueError, IndexError):
                pass
    return pids


def pss_mib(pids):
    """PSS splits shared pages between processes, so several aren't over-counted."""
    total = 0
    for pid in pids:
        try:
            for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
                if line.startswith("Pss:"):
                    total += int(line.split()[1])
        except OSError:
            pass
    return total / 1024


def cpu_ticks(pids):
    total = 0
    for pid in pids:
        try:
            fields = stat_fields(pid)
            total += int(fields[11]) + int(fields[12])
        except (OSError, ValueError, IndexError):
            pass
    return total


def stop(sid, proc):
    try:
        os.killpg(sid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    time.sleep(3)
    for pid in session_pids(sid):
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    proc.wait()


def measure(app, program, round_number):
    with tempfile.TemporaryDirectory(prefix=f"{app}-bench-") as home:
        env = dict(os.environ)
        env.update(
            HOME=home,
            XDG_CONFIG_HOME=f"{home}/.config",
            XDG_DATA_HOME=f"{home}/.local/share",
            XDG_CACHE_HOME=f"{home}/.cache",
            XDG_STATE_HOME=f"{home}/.local/state",
            IRORI_HOME=f"{home}/irori",
        )
        proc = subprocess.Popen(
            [program], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, start_new_session=True,
        )
        sid = proc.pid
        try:
            time.sleep(SETTLE_SECONDS)
            before = cpu_ticks(session_pids(sid))
            start = time.monotonic()
            time.sleep(IDLE_SECONDS)
            pids = session_pids(sid)
            used = (cpu_ticks(pids) - before) / TICKS_PER_SECOND
            return {
                "app": app,
                "round": round_number,
                "running": proc.poll() is None,
                "processes": len(pids),
                "pss_mib": round(pss_mib(pids), 1),
                "idle_cpu_percent": round(used / (time.monotonic() - start) * 100, 3),
            }
        finally:
            stop(sid, proc)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--irori", default="irori", help="Irori program")
    parser.add_argument("--heroic", default="heroic", help="Heroic program")
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--output", default="results.csv", help="CSV file to write")
    args = parser.parse_args()

    apps = {"irori": args.irori, "heroic": args.heroic}
    rows = []
    for number in range(1, args.rounds + 1):
        # Alternate the order so a warm disk cache doesn't always favour the same app.
        for app in sorted(apps, reverse=number % 2 == 1):
            rows.append(measure(app, apps[app], number))
            print(rows[-1], flush=True)

    with open(args.output, "w", newline="") as file:
        writer = csv.DictWriter(file, fieldnames=rows[0].keys())
        writer.writeheader()
        writer.writerows(rows)

    for app in apps:
        memory = statistics.median(r["pss_mib"] for r in rows if r["app"] == app)
        print(f"{app}: median {memory} MiB")


if __name__ == "__main__":
    main()

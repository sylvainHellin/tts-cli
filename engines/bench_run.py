#!/usr/bin/env python3
"""Resource-benchmark wrapper for a local TTS engine run.

Usage:
    bench_run.py -- <engine_cmd> [engine_args...]

Launches the engine command as a subprocess, samples its process tree for peak
RSS / peak thread count / iGPU busy percent every ~150 ms, and after exit reads
authoritative CPU seconds from getrusage(RUSAGE_CHILDREN). It passes the engine's
single stdout JSON line through (the CONTRACT.md line with audio_seconds etc.)
and prints ONE combined JSON object to stdout:

  {
    "wall_seconds": <f>,
    "cpu_user_s": <f>,
    "cpu_sys_s": <f>,
    "peak_rss_bytes": <int>,   # USS-based unique-memory estimate (RSS fallback)
    "peak_threads": <int>,
    "igpu_busy_peak": <f|null>,
    "igpu_busy_mean": <f|null>,
    "engine_json": { ... }   # the engine's own contract line, or null
  }

All diagnostics go to STDERR so stdout stays a single clean JSON line.
"""

import glob
import json
import os
import resource
import subprocess
import sys
import threading
import time
from pathlib import Path

SAMPLE_INTERVAL_S = 0.15


def log(*args):
    print(*args, file=sys.stderr, flush=True)


def find_igpu_busy_file():
    """First existing AMD iGPU busy-percent sysfs file, or None."""
    for path in sorted(glob.glob("/sys/class/drm/card*/device/gpu_busy_percent")):
        if os.path.isfile(path):
            return path
    return None


def read_igpu_busy(path):
    try:
        with open(path, "r") as fh:
            return float(fh.read().strip())
    except (OSError, ValueError):
        return None


class TreeSampler(threading.Thread):
    """Poll a process tree for peak RSS, peak thread count, and iGPU busy%."""

    def __init__(self, root_pid, igpu_file):
        super().__init__(daemon=True)
        self.root_pid = root_pid
        self.igpu_file = igpu_file
        self.peak_rss = 0
        self.peak_threads = 0
        self.igpu_samples = []
        self._stop_evt = threading.Event()
        try:
            import psutil  # noqa: F401
            self._psutil_ok = True
        except Exception as exc:  # pragma: no cover - env guard
            log(f"[bench] psutil unavailable: {exc}; RSS/thread sampling disabled")
            self._psutil_ok = False

    def _sample_tree(self):
        import psutil
        try:
            root = psutil.Process(self.root_pid)
        except psutil.Error:
            return
        procs = [root]
        try:
            procs += root.children(recursive=True)
        except psutil.Error:
            pass
        total_rss = 0
        max_threads = 0
        for p in procs:
            try:
                # Prefer USS (unique set size) so shared pages mapped into
                # multiple processes (e.g. torch/onnxruntime .so) are not
                # double-counted across the tree. Fall back to RSS when USS is
                # unavailable (e.g. AccessDenied or unsupported platform).
                try:
                    total_rss += p.memory_full_info().uss
                except (psutil.Error, AttributeError):
                    total_rss += p.memory_info().rss
                max_threads = max(max_threads, p.num_threads())
            except psutil.Error:
                continue
        self.peak_rss = max(self.peak_rss, total_rss)
        self.peak_threads = max(self.peak_threads, max_threads)

    def run(self):
        while not self._stop_evt.is_set():
            if self._psutil_ok:
                self._sample_tree()
            if self.igpu_file:
                v = read_igpu_busy(self.igpu_file)
                if v is not None:
                    self.igpu_samples.append(v)
            self._stop_evt.wait(SAMPLE_INTERVAL_S)

    def stop(self):
        self._stop_evt.set()


def main() -> int:
    argv = sys.argv[1:]
    if argv and argv[0] == "--":
        argv = argv[1:]
    if not argv:
        log("[bench] error: no engine command given (expected: bench_run.py -- <cmd> ...)")
        return 2

    igpu_file = find_igpu_busy_file()
    if igpu_file:
        log(f"[bench] sampling iGPU from {igpu_file}")
    else:
        log("[bench] no iGPU busy sysfs found; igpu metrics will be null")

    log(f"[bench] launching: {' '.join(argv)}")
    # Engine stdout is captured (its one JSON line); stderr streams straight through.
    start = time.monotonic()
    proc = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=None, text=True)

    sampler = TreeSampler(proc.pid, igpu_file)
    sampler.start()

    engine_stdout, _ = proc.communicate()
    wall = time.monotonic() - start
    sampler.stop()
    sampler.join(timeout=2.0)

    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    # ru_maxrss is KiB on Linux; use it as a floor for peak RSS in case sampling
    # missed the peak of a short run.
    rusage_rss = usage.ru_maxrss * 1024
    peak_rss = max(sampler.peak_rss, rusage_rss)

    engine_json = None
    if engine_stdout:
        # The engine may emit a stray line; take the last non-empty line that parses.
        for line in reversed(engine_stdout.strip().splitlines()):
            line = line.strip()
            if not line:
                continue
            try:
                engine_json = json.loads(line)
                break
            except json.JSONDecodeError:
                continue
        if engine_json is None:
            log(f"[bench] warning: engine stdout was not valid JSON: {engine_stdout!r}")

    igpu_peak = max(sampler.igpu_samples) if sampler.igpu_samples else None
    igpu_mean = (
        sum(sampler.igpu_samples) / len(sampler.igpu_samples)
        if sampler.igpu_samples
        else None
    )

    if proc.returncode != 0:
        log(f"[bench] engine exited non-zero: {proc.returncode}")

    record = {
        "wall_seconds": round(wall, 3),
        "cpu_user_s": round(usage.ru_utime, 3),
        "cpu_sys_s": round(usage.ru_stime, 3),
        # USS-based unique-memory estimate summed over the process tree
        # (falls back to RSS per-process where USS is unavailable); floored by
        # RUSAGE_CHILDREN ru_maxrss. Field name kept for compatibility.
        "peak_rss_bytes": int(peak_rss),
        "peak_threads": int(sampler.peak_threads),
        "igpu_busy_peak": igpu_peak,
        "igpu_busy_mean": round(igpu_mean, 2) if igpu_mean is not None else None,
        "engine_json": engine_json,
    }
    print(json.dumps(record))
    return proc.returncode


if __name__ == "__main__":
    sys.exit(main())

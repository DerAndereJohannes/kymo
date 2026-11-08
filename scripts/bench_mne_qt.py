#!/usr/bin/env python3
"""Time mne-qt-browser on the views kymo's frames benchmark drew.

Usage:
    cargo bench -p kymo --bench frames -- FILE --views views.tsv
    python scripts/bench_mne_qt.py FILE views.tsv

Needs mne, mne-qt-browser and a Qt binding. Without installing them, run it through uv:
    uv run --no-project --with mne --with mne-qt-browser --with pyqt6 \\
        python scripts/bench_mne_qt.py FILE views.tsv

The file opens with mne.io.read_raw (preload=False) and raw.plot() with mne-qt-browser's
defaults (or the settings stored in its preferences) but without antialiasing, which is
very slow on macOS, at kymo's first view (20 s, 20 channels), in a 1280x800 window. Each view from
views.tsv (phase, start, span, first row, rows) is then set with setXRange/setYRange, and
timed until a forced repaint has finished: mne-qt-browser reads and draws a view's data
synchronously, so that is both its frame time and its step latency.

A view that takes longer than --step-limit ends its phase; the rest of the phase is
counted as skipped. The browser runs in a child process, stopped after --time-limit or when
its resident memory passes --memory-limit; its peak memory is read when it ends, so it is
known for stopped runs too.
"""

import argparse
import math
import os
import resource
import subprocess
import sys
import threading
import time

def read_views(path):
    views = []
    with open(path) as f:
        next(f)
        for line in f:
            phase, start, span, top, rows = line.rstrip("\n").split("\t")
            views.append((phase, float(start), float(span), float(top), int(rows)))
    return views


def child(path, views_path, step_limit):
    """Runs the browser and prints one line per measurement, flushed as it goes."""
    import mne
    import mne_qt_browser
    from qtpy.QtWidgets import QApplication

    def emit(*fields):
        print("\t".join(str(f) for f in fields), flush=True)

    views = read_views(views_path)
    phase, start, span, top, rows = views[0]
    assert phase == "opening", views_path
    mne.viz.set_browser_backend("qt")
    t0 = time.perf_counter()
    raw = mne.io.read_raw(path, preload=False, verbose="error")
    fig = raw.plot(duration=span, n_channels=rows, start=start, show=False, block=False)
    # Antialiasing is very slow on macOS. mne-qt-browser saves its settings on closing, so the
    # stored value goes back before that.
    stored_antialiasing = fig.mne.antialiasing
    if fig.mne.antialiasing:
        fig._toggle_antialiasing()
    fig.resize(1280, 800)
    fig.show()
    app = QApplication.instance()
    app.processEvents()
    fig.mne.view.viewport().repaint()
    emit("open", time.perf_counter() - t0)
    emit(
        "info",
        f"mne {mne.__version__}, mne-qt-browser {mne_qt_browser.__version__}, precompute "
        f"{fig.mne.enable_precompute}, OpenGL {fig.mne.use_opengl}, antialiasing "
        f"{fig.mne.antialiasing} (stored: {stored_antialiasing}), downsampling "
        f"{fig.mne.downsampling} ({fig.mne.ds_method})",
    )

    shown = (start, span, top, rows)
    stopped = None
    for phase, start, span, top, rows in views[1:]:
        if phase == stopped:
            emit("skip", phase)
            continue
        t0 = time.perf_counter()
        if (start, span) != shown[:2]:
            fig.mne.plt.setXRange(start, start + span, padding=0)
        if (top, rows) != shown[2:]:
            fig.mne.plt.setYRange(top, top + rows + 1, padding=0)
        app.processEvents()
        fig.mne.view.viewport().repaint()
        seconds = time.perf_counter() - t0
        shown = (start, span, top, rows)
        emit("view", phase, seconds)
        if seconds > step_limit:
            stopped = phase
    fig.mne.antialiasing = stored_antialiasing
    fig.close()


def stats(values):
    """Count, p50, p99 and max (nearest rank), as kymo's benchmark reports them."""
    values = sorted(values)
    if not values:
        return 0, math.nan, math.nan, math.nan

    def rank(p):
        return values[max(math.ceil(p * len(values)), 1) - 1]

    return len(values), rank(0.5), rank(0.99), values[-1]


def ms(seconds):
    if math.isnan(seconds):
        return "–"
    if seconds >= 10:
        return f"{seconds:.0f} s"
    return f"{seconds * 1e3:.1f} ms"


def resident_bytes(pid):
    """Current resident memory of `pid`, from ps (macOS, Linux)."""
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True)
    return int(out.stdout.strip() or 0) * 1024


def peak_footprint_bytes(pid):
    """Peak physical footprint of `pid` so far (macOS only, else None), as Activity
    Monitor's Memory column counts it: resident memory without clean file pages."""
    if sys.platform != "darwin":
        return None
    import ctypes

    class RusageInfoV4(ctypes.Structure):
        # struct rusage_info_v4 from <sys/resource.h>, up to the field needed.
        _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [
            (name, ctypes.c_uint64)
            for name in (
                "user_time diskio_bytesread system_time pkg_idle_wkups interrupt_wkups "
                "pageins wired_size resident_size phys_footprint proc_start_abstime "
                "proc_exit_abstime child_user_time child_system_time child_pkg_idle_wkups "
                "child_interrupt_wkups child_pageins child_elapsed_abstime "
                "diskio_byteswritten cpu_time_qos_default cpu_time_qos_maintenance "
                "cpu_time_qos_background cpu_time_qos_utility cpu_time_qos_legacy "
                "cpu_time_qos_user_initiated cpu_time_qos_user_interactive "
                "billed_system_time serviced_system_time logical_writes "
                "lifetime_max_phys_footprint instructions cycles billed_energy "
                "serviced_energy interval_max_phys_footprint runnable_time"
            ).split()
        ]

    libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
    info = RusageInfoV4()
    if libproc.proc_pid_rusage(pid, 4, ctypes.byref(info)) != 0:
        return None
    return info.lifetime_max_phys_footprint


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("file")
    parser.add_argument("views")
    parser.add_argument("--step-limit", type=float, default=30, help="seconds (30)")
    parser.add_argument("--time-limit", type=float, default=600, help="seconds (600)")
    ram = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    parser.add_argument(
        "--memory-limit",
        type=float,
        default=round(ram * 0.75 / 1e9),
        help="GB (3/4 of RAM)",
    )
    parser.add_argument("--child", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.child:
        child(args.file, args.views, args.step_limit)
        return

    command = [
        sys.executable,
        __file__,
        args.file,
        args.views,
        "--step-limit",
        str(args.step_limit),
        "--child",
    ]
    proc = subprocess.Popen(command, stdout=subprocess.PIPE, text=True)
    stop = []
    footprint = [None]

    def watch():
        started = time.monotonic()
        while proc.poll() is None:
            # Read while the child lives: the kernel keeps its peak until it exits.
            footprint[0] = peak_footprint_bytes(proc.pid) or footprint[0]
            if time.monotonic() - started > args.time_limit:
                stop.append(f"stopped after {args.time_limit:.0f} s")
            elif resident_bytes(proc.pid) > args.memory_limit * 1e9:
                stop.append(f"stopped at {args.memory_limit:.0f} GB resident")
            if stop:
                proc.kill()
                return
            time.sleep(0.2)

    watcher = threading.Thread(target=watch, daemon=True)
    watcher.start()
    times, skipped, opening, info = [], [], None, None
    for line in proc.stdout:
        kind, *fields = line.rstrip("\n").split("\t")
        if kind == "open":
            opening = float(fields[0])
        elif kind == "info":
            info = fields[0]
        elif kind == "view":
            times.append((fields[0], float(fields[1])))
        elif kind == "skip":
            skipped.append(fields[0])
    proc.wait()
    watcher.join()
    peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    peak *= 1 if sys.platform == "darwin" else 1024

    print(args.file)
    if info:
        print(info)
    print(f"first view after {ms(opening)}" if opening is not None else "never opened")
    print(f"{'':<10} {'views':>6} {'p50':>9} {'p99':>9} {'max':>9}   {'skipped':>8}")
    phases = list(dict.fromkeys(phase for phase, *_ in read_views(args.views)[1:]))
    for name, chosen in [(p, [p]) for p in phases] + [("all", phases)]:
        n, p50, p99, top = stats([s for p, s in times if p in chosen])
        skips = sum(p in chosen for p in skipped)
        print(f"{name:<10} {n:>6} {ms(p50):>9} {ms(p99):>9} {ms(top):>9}   {skips:>8}")
    print(
        "views: from setting the range to the end of a forced repaint; skipped: views after "
        f"one that took over {args.step_limit:.0f} s, in the same phase."
    )
    if stop:
        print(stop[0] + "; the views not reached are missing above.")
    elif proc.returncode != 0:
        print(f"the browser exited with status {proc.returncode}")
    print(f"peak resident memory: {peak / 1e6:.0f} MB")
    if footprint[0] is not None:
        print(
            f"peak physical footprint: {footprint[0] / 1e6:.0f} MB (without file pages the "
            "system can drop; sampled every 0.2 s)"
        )


if __name__ == "__main__":
    main()

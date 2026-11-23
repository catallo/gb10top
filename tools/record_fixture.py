#!/usr/bin/env python3
"""Record the /proc and /sys files gb10top reads into a fixture tree.

Usage: tools/record_fixture.py tests/fixtures/NAME

The tree can then be read back with `gb10top --root tests/fixtures/NAME` and
is what the fixture tests run against. Only hardware state is recorded: the
host name is replaced, no MAC addresses are read, virtual network interfaces
are skipped, and the process list is two made-up processes.
"""
import glob
import os
import sys

out = sys.argv[1]


def put(path, text):
    dest = os.path.join(out, path.lstrip("/"))
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    with open(dest, "w") as f:
        f.write(text)


def copy(path):
    try:
        with open(path) as f:
            put(path, f.read())
    except OSError:
        pass  # unreadable or absent on this machine: leave it out


def copy_all(pattern):
    for p in sorted(glob.glob(pattern)):
        if os.path.isfile(p):
            copy(p)


for f in ("sys_vendor", "product_name"):
    copy(f"/sys/class/dmi/id/{f}")
for f in ("stat", "meminfo", "loadavg", "uptime", "pressure/cpu", "pressure/memory", "pressure/io"):
    copy(f"/proc/{f}")
put("/proc/sys/kernel/hostname", "fixture\n")

for cpu in glob.glob("/sys/devices/system/cpu/cpu[0-9]*"):
    for f in ("cpufreq/cpuinfo_max_freq", "cpufreq/scaling_cur_freq", "cpufreq/scaling_governor",
              "cache/index3/shared_cpu_list"):
        copy(f"{cpu}/{f}")

# hwmon and thermal entries are symlinks in /sys/class; record them as plain directories.
for hw in glob.glob("/sys/class/hwmon/hwmon*"):
    copy(f"{hw}/name")
    for pat in ("*_input", "*_label", "*_cap", "*_min", "*_max", "pl_level", "prochot", "tj_max_c"):
        copy_all(f"{hw}/{pat}")
for z in glob.glob("/sys/class/thermal/thermal_zone*"):
    for f in ("type", "temp", "trip_point_0_type", "trip_point_0_temp"):
        copy(f"{z}/{f}")
for c in glob.glob("/sys/class/thermal/cooling_device*"):
    for f in ("type", "cur_state", "max_state"):
        copy(f"{c}/{f}")

for blk in glob.glob("/sys/block/*"):
    if os.path.exists(f"{blk}/device"):
        copy(f"{blk}/stat")
        copy(f"{blk}/device/model")

for net in glob.glob("/sys/class/net/*"):
    if not os.path.exists(f"{net}/device"):
        continue  # virtual interface
    for f in ("operstate", "speed", "statistics/rx_bytes", "statistics/tx_bytes", "statistics/rx_errors",
              "statistics/tx_errors", "statistics/rx_dropped", "statistics/tx_dropped"):
        copy(f"{net}/{f}")
    # Keep the driver name as a (dangling) symlink, which is how the app reads it.
    driver = os.path.basename(os.path.realpath(f"{net}/device/driver"))
    link = os.path.join(out, net.lstrip("/"), "device/driver")
    os.makedirs(os.path.dirname(link), exist_ok=True)
    if not os.path.lexists(link):
        os.symlink(f"../../../../bus/pci/drivers/{driver}", link)

for dev in glob.glob("/sys/bus/pci/devices/*"):
    for f in ("class", "current_link_speed", "current_link_width", "max_link_speed", "max_link_width"):
        copy(f"{dev}/{f}")

# Two made-up processes: one plain, one inside a container.
STAT = "{pid} ({name}) S 1 {pid} {pid} 0 -1 4194560 100 0 0 0 {utime} {stime} 0 0 20 0 3 0 1000 123456 {rss} 18446744073709551615 0 0\n"
put("/proc/100/stat", STAT.format(pid=100, name="worker", utime=250, stime=50, rss=4096))
put("/proc/100/cgroup", "0::/user.slice/user-1000.slice/session-1.scope\n")
put("/proc/200/stat", STAT.format(pid=200, name="model server", utime=9000, stime=1000, rss=65536))
put("/proc/200/cgroup", "0::/system.slice/docker-" + "ab" * 32 + ".scope\n")

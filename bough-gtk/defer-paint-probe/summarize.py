#!/usr/bin/env python3
"""Condenses one or more `defer-paint-probe all` outputs into a table:
one line per scenario round and variant, with the numbers of each run
side by side (painted frames / frames with wrong text)."""
import re
import sys

ROW = re.compile(
    r"^  (?P<variant>control|deferred) +(?P<round>.+?) +frames +(?P<frames>\d+) +painted +(?P<painted>\d+)"
    r" +wrong +(?P<wrong>\d+) \(rows +(?P<empty>\d+) empty, +(?P<stale>\d+) stale, +(?P<unbound>\d+) unbound, of +(?P<checked>\d+);"
    r" most px of a wrong label in view +(?P<px>\d+), wholly in view +(?P<whole>\d+)(?:, found by pick +(?P<picked>\d+))?(?:; blank frames (?P<blank>\d+))?\)"
    r" +binds \[(?P<binds>[^\]]*)\](?: of them in view \[(?P<inview>[^\]]*)\])? +writes \[(?P<writes>[^\]]*)\] dropped (?P<dropped>\d+)"
    r" +polls-in-clock (?P<polls>\d+) +pick-check (?P<misses>\d+)/(?P<picks>\d+) +max-step (?P<step>\d+)"
)


def parse(path):
    scenario = None
    out = {}
    for line in open(path):
        line = line.rstrip("\n")
        if line.startswith("=== "):
            scenario = line[4:]
            continue
        m = ROW.match(line)
        if m:
            key = (scenario, m["round"], m["variant"])
            out[key] = m.groupdict()
    return out


def main():
    runs = [parse(p) for p in sys.argv[1:]]
    keys = []
    for run in runs:
        for k in run:
            if k not in keys:
                keys.append(k)
    print(f"{'scenario / round':<62} {'variant':<8} " + "  ".join(f"run{i + 1}: painted/wrong  rows(e/s)  px whole  binds  writes  step" for i in range(len(runs))))
    for k in keys:
        cells = []
        for run in runs:
            r = run.get(k)
            if r is None:
                cells.append("-")
                continue
            cells.append(
                f"{r['painted']:>4}/{r['wrong']:<3} ({r['empty']}/{r['stale']}) px{r['px']} w{r['whole']}"
                + (f" pick{r['picked']}" if r.get("picked") is not None else "")
                + (f" blank{r['blank']}" if r.get("blank") not in (None, "0") else "")
                + f" [{r['binds']}]"
                + (f" in-view[{r['inview']}]" if r.get("inview") is not None else "")
                + f" [{r['writes']}] step{r['step']}"
                + (f" PICK-MISS {r['misses']}" if r["misses"] != "0" else "")
                + (f" POLLS-IN-CLOCK {r['polls']}" if r["polls"] != "0" else "")
            )
        print(f"{k[0] + ' / ' + k[1]:<62} {k[2]:<8} " + "  |  ".join(cells))


main()

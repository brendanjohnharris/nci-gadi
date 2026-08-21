# monitor (Gadi edition)

A terminal UI for watching your PBS jobs on NCI Gadi, with a tabbed view
(Job Usage / Log Preview / Details / Processes), per-job log preview, resource
and efficiency columns, SU + storage accounting, node states, and recent-job
exit statuses.
Ported from the physics-cluster
[`build/monitor`](https://github.com/brendanjohnharris/physics-cluster/tree/main/build/monitor)
and tailored to Gadi (see [Differences from the original](#differences-from-the-original)).
It polls PBS gently (see [Polling](#polling)).

## Requirements

- Nothing beyond a Gadi login (or compute) node: `qstat` and `qcat` come from
  `/opt/pbs/default/bin`, which is already on PATH.
- To build: a Rust toolchain. `build.sh` installs one automatically (into
  `/scratch`, not your 10GB home) if none is found.

## Build & install

```bash
cd ~/build/monitor
./build.sh          # add --test to run the test suite first
```

This builds `monitor`, `qusage`, and `qarray` and **copies** them into
`~/.local/bin` (backing up any pre-existing commands of the same name to
`*.bak` once). The toolchain and build artifacts live under
`/scratch/$PROJECT/$USER/{rust,build/monitor-target}` to spare the home quota;
because /scratch purges files unaccessed for 100 days, the installed binaries
are copies (they keep working even if the build tree is purged — rerun
`build.sh` to get a toolchain back).

## Usage

```
monitor [-u USERNAME] [--usage-interval SECS] [--jobs-interval SECS]
        [--qcat-interval SECS] [--account-interval SECS]
```

- `-u` / `--username` — user to monitor (default: `$USER`). Switch live at any
  time with the `u` key.
- `--usage-interval` — per-job usage poll interval, seconds (default: 30).
- `--jobs-interval` — your-jobs poll interval, seconds (default: 10).
- `--qcat-interval` — spooled-output poll interval, seconds (default: 15).
- `--account-interval` — SU-accounting / scratch-expiry poll interval, seconds
  (default: 600). These always track *your* `$PROJECT`, even when monitoring
  another user.

### Keys

| Keys | Action |
|---|---|
| `←` / `→` | switch top tab (Job Usage / Log Preview / Details / Processes) |
| `↑`/`k`, `↓`/`j` | scroll the active tab by one line |
| `PgUp` / `PgDn` | scroll the active tab by one page |
| `Home`/`g`, `End`/`G` | jump to top / bottom of the active tab |
| `,` / `.` | previous / next running job (drives the Log Preview, Details and Processes tabs) |
| `a` / `q` / `r` / `f` | toggle the Array / Queued&Held / Running / Recent sections |
| `e` / `c` | toggle array-job compaction (collapse subjobs to `<num>[]`); both keys do the same |
| `u` | switch the monitored user (type a fragment; it fuzzy-matches members of your project groups, `→` shows the match; Enter confirms, Esc cancels; a full username outside your projects also works) |
| `Ctrl-R` | refresh PBS data now |
| `Esc` / `Ctrl-C` | quit |

Sections appear only when they have data and auto-size to their content. The
Job Usage pane reserves at most 2/3 of the terminal whenever any bottom
section has data, so a tall panel (many jobs) can never squeeze the job lists
off the screen; past the cap the tab scrolls. It is top-anchored (the quota
chart and table header stay put) — `End`/`G` jumps to its bottom, `Home`/`g`
back to the top. If space is still tight after the cap, the RECENT section is
dropped first (it's history; the running/queued lists are live). Job ids
are shown without the `.gadi-pbs` suffix and node names without the `gadi-`
prefix. On a wide enough terminal the Running and Queued & Held sections lay
their rows out in as many columns as fit (separated by a `┃` divider coloured to
match the section). When there are more jobs than fit, the two sections split
the space equally, each truncating with a "N hidden" note, and the view
auto-compacts array jobs to a single `<num>[] ×N` row (toggle with `e`/`c`).
Suspended (`S`) jobs are listed in the Queued & Held section with state `S`.

### The Job Usage tab

The tab opens with a small slow-cadence quota chart: one meter row for the
quarter's SU usage (from `nci_account`), one for your home directory (from
`quota`), then one per project filesystem — each with a space meter and,
where a real inode quota exists, an inode meter (home's inode quota is
effectively unlimited on Gadi, so it shows the bare count). Meters are
coloured green/yellow/red by fullness (75%/90% thresholds), and a
nonzero-but-tiny fraction always draws a sliver:

```
SU (2026.q3)  ▕▁         ▏   0% · 297.2K of 299.0K avail
home          ▕▇▇▇▇▇▇▇   ▏  70% · 7.0G/10.0G    files 91.0K
gdata         ▕▂         ▏   2% · 237.9M/10.0G  files ▕▇▇▅       ▏  26% · 9.3K/36.0K
scratch       ▕▁         ▏   1% · 10.1G/1.0T    files ▕▇▇▄       ▏  25% · 50.4K/202.0K
```

A red `⚠ … scheduled for expiry` line appears beneath the chart if
`nci-file-expiry list-warnings` reports scratch files nearing the 100-day
purge.

Then, for each running job, one `qstat -f` per poll yields:

| Column | Meaning |
|---|---|
| `CPU%` | instantaneous per-core utilisation (`cpupercent / ncpus`) |
| `EFF%` | whole-run efficiency (`cput / (walltime × ncpus)` — nqstat_anu's %CPU) |
| `GPU%` | per-GPU utilisation (`gpu_util / ngpus`; column appears only for GPU jobs) |
| `MEM` | used / requested memory |
| `JOBFS` | used / requested jobfs (column appears when ≥1GB was requested) |
| `WALLTIME` | elapsed / requested, with a progress bar |

Colours: utilisation green when high (you're using what you're charged for);
memory red near the request (PBS kills the job past it) and yellow when far
below it (memory is charged at 1 core per 4GB, so over-requesting burns SUs);
jobfs only goes red near its limit (low use is normal); the walltime bar goes
yellow at 75% and red at 90% of the limit. Usage fields show `--` for the
first minutes of a job, until PBS's first accounting update.

Below the running table:

- **QUEUED** — one line per waiting job (array subjobs collapse to their `[]`
  master): its state letter, the scheduler's `estimated.start_time` when
  computed, and the `comment` explaining why it isn't running
  (`Not Running: Insufficient amount of resource: ncpus`, …).
- **NODES** — `pbsnodes` on the nodes your running jobs occupy: state
  (free/job-busy/down), assigned/available cores and memory, GPU count, and
  how many jobs share the node (`29 jobs · 1 mine`).
- **queues:** — running/queued totals (from `qstat -Q`) for each queue you
  have jobs in, e.g. `copyq 66R/4Q` — a feel for the backlog you're behind.

### The Processes tab

A `ps` of the selected running job's actual processes, fetched via Gadi's
`qps` (per compute node; `qps_gpu` — same listing plus GPU utilisation — is
tried first for jobs in gpu queues). Fetched when you enter the tab or switch
jobs; Ctrl-R re-fetches. Answers "is it actually computing, or stuck?"

### The RECENT JOBS section

The last few finished jobs from Gadi's job history (`qstat -x`), newest
first: walltime used, exit status (`ok` green; `exit N` / `sig N` red — e.g.
`sig 15` for a qdel, `sig 9` for an OOM kill), and when each finished. Toggle
with `f`. Answers "did my run die overnight?" at a glance.

### Log Preview

Resolved per selected job, in priority order:

1. `~/.jobs/<jobid>.log` (or `~/.jobs/<num>[]*/<task>.log` for array tasks) —
   the "tee your output" convention, tailed live via inotify;
2. the job's `Output_Path` file, if it exists on disk — i.e. you submitted with
   `#PBS -k oed` (streams the `.o` file live), or the job just finished and PBS
   copied it back;
3. otherwise **`qcat`**: the spooled stdout of the running job, re-fetched every
   `--qcat-interval` seconds.

When a job ends, qcat stops working and the preview switches to the copied-back
`.o` file automatically (within a couple of seconds of it appearing).

### Standalone `qusage`

The Job Usage panel as its own command, printing the summary, per-job usage,
queued diagnosis, node states and queue pressure once and exiting (the Gadi
stand-in for the original's cluster-wide `qlload`):

```
qusage [-u USERNAME] [-a|--account] [-r|--recent]
```

- `-a` — prepend the SU + storage report (runs `nci_account`; adds a few seconds).
- `-r` — append the RECENT JOBS panel (finished jobs with exit status).

### Standalone `qarray`

Array progress as its own command: a progress bar per array job
(completed/running subjobs, plus an ETA from the mean subjob walltime):

```
qarray [-u USERNAME]
```

## Polling

The render loop never queries PBS; a background scheduler polls on its own
cadence and the UI draws from a cache. Your jobs come from a single
`qstat -w -u <user> -t -n1` per jobs-interval. Per usage-interval: one
`qstat -f` over the running ids (capped at 24) plus waiting masters (capped
at 8, arrays collapsed to `[]`), one `pbsnodes` over the occupied nodes
(capped at 12), one `qstat -Q`, and the RECENT panel's `qstat -xw -u` +
`qstat -fx` over its last ≤8 finished ids. Per account-interval (10 min):
`nci_account` and `nci-file-expiry list-warnings`. The Details tab runs
`qstat -f <jobid>` (falling back to `-xf` history) once per job selection
(rapid switching coalesces to a single fetch); the Processes tab runs `qps`
on demand; array progress is `qstat -w -t <base>` per array master; the log
preview is inotify-driven for files and polled for qcat. All commands run
locally — no SSH.

## Differences from the original

Gadi-specific changes from the physics-cluster version:

- **No SSH layer.** PBS client commands work on Gadi login and compute nodes,
  so everything runs locally (`ssh.rs` is gone; no `~/.ssh/config` edits).
- **`qlload` → `qusage`.** The cluster-wide node graph (`qstat -ft` +
  `pbsnodes -av`) is neither permitted nor sensible against Gadi's thousands of
  nodes. The System Load tab is now Job Usage: your running jobs' CPU/memory/
  GPU/walltime usage.
- **`qcat` log fallback.** Gadi spools job output on the compute node until the
  job ends; `qcat` (an NCI tool) reads that spool, so the Log Preview works for
  running jobs without any `~/.jobs` convention or `-k oed`.
- **`u` fuzzy candidates** come from `getent group` over your unix groups
  (project co-members) instead of "everyone with a job on the cluster". Note
  Gadi hides other users' jobs from plain `qstat`, so switching to another
  user currently shows rows only if PBS lets you see them.
- **NCI data sources beyond PBS**: `nci_account` (SU + storage header),
  `nci-file-expiry` (scratch purge warnings), `qps`/`qps_gpu` (Processes
  tab), `pbsnodes` (NODES block), `qstat -Q` (queue pressure), and `qstat -x`
  job history (RECENT section with exit statuses).
- Job ids strip `.gadi-pbs`, nodes strip `gadi-`, queues strip `-exec`;
  suspended (`S`) jobs are shown; `qstat -f` requests are lenient about ids
  that vanish mid-poll; `Resource_List.mem`'s raw-bytes form is handled.

## Uninstall

```bash
cd ~/.local/bin && rm -f monitor qusage qarray
# restore anything that was backed up:
for f in monitor.bak qusage.bak qarray.bak; do [ -e "$f" ] && mv "$f" "${f%.bak}"; done
```

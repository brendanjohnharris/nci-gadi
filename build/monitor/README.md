# monitor (Gadi edition)

A terminal UI for watching your PBS jobs on NCI Gadi, with a tabbed view
(Job Usage / Log Preview / Details), per-job log preview, and resource columns.
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
monitor [-u USERNAME] [--usage-interval SECS] [--jobs-interval SECS] [--qcat-interval SECS]
```

- `-u` / `--username` — user to monitor (default: `$USER`). Switch live at any
  time with the `u` key.
- `--usage-interval` — per-job usage poll interval, seconds (default: 30).
- `--jobs-interval` — your-jobs poll interval, seconds (default: 10).
- `--qcat-interval` — spooled-output poll interval, seconds (default: 15).

### Keys

| Keys | Action |
|---|---|
| `←` / `→` | switch top tab (Job Usage / Log Preview / Details) |
| `↑`/`k`, `↓`/`j` | scroll the active tab by one line |
| `PgUp` / `PgDn` | scroll the active tab by one page |
| `Home`/`g`, `End`/`G` | jump to top / bottom of the active tab |
| `,` / `.` | previous / next running job (drives the Log Preview and Details tabs) |
| `a` / `q` / `r` | toggle the Array / Queued&Held / Running sections |
| `e` / `c` | toggle array-job compaction (collapse subjobs to `<num>[]`); both keys do the same |
| `u` | switch the monitored user (type a fragment; it fuzzy-matches members of your project groups, `→` shows the match; Enter confirms, Esc cancels; a full username outside your projects also works) |
| `Ctrl-R` | refresh PBS data now |
| `Esc` / `Ctrl-C` | quit |

Sections appear only when they have data and auto-size to their content. Job ids
are shown without the `.gadi-pbs` suffix and node names without the `gadi-`
prefix. On a wide enough terminal the Running and Queued & Held sections lay
their rows out in as many columns as fit (separated by a `┃` divider coloured to
match the section). When there are more jobs than fit, the two sections split
the space equally, each truncating with a "N hidden" note, and the view
auto-compacts array jobs to a single `<num>[] ×N` row (toggle with `e`/`c`).
Suspended (`S`) jobs are listed in the Queued & Held section with state `S`.

### The Job Usage tab

For each running job, one `qstat -f` per poll yields:

| Column | Meaning |
|---|---|
| `CPU%` | instantaneous per-core utilisation (`cpupercent / ncpus`) |
| `EFF%` | whole-run efficiency (`cput / (walltime × ncpus)` — nqstat_anu's %CPU) |
| `GPU%` | per-GPU utilisation (`gpu_util / ngpus`; column appears only for GPU jobs) |
| `MEM` | used / requested memory |
| `WALLTIME` | elapsed / requested, with a progress bar |

Colours: utilisation green when high (you're using what you're charged for);
memory red near the request (PBS kills the job past it) and yellow when far
below it (memory is charged at 1 core per 4GB, so over-requesting burns SUs);
the walltime bar goes yellow at 75% and red at 90% of the limit. Usage fields
show `--` for the first minutes of a job, until PBS's first accounting update.

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

The Job Usage panel as its own command, printing the summary + per-job usage
once and exiting (the Gadi stand-in for the original's cluster-wide `qlload`):

```
qusage [-u USERNAME]
```

### Standalone `qarray`

Array progress as its own command: a progress bar per array job
(completed/running subjobs, plus an ETA from the mean subjob walltime):

```
qarray [-u USERNAME]
```

## Polling

The render loop never queries PBS; a background scheduler polls on its own
cadence and the UI draws from a cache. Your jobs come from a single
`qstat -w -u <user> -t -n1` per jobs-interval; the usage panel from one
`qstat -f <running-ids…>` per usage-interval (capped at 32 jobs, reusing the
job list rather than re-querying); the Details tab from `qstat -f <jobid>`
(falling back to `-xf` history for finished jobs), fetched once per job
selection (rapid switching coalesces to a single fetch); array progress from
`qstat -w -t <base>` per array master; the log preview is inotify-driven for
files and polled for qcat. All commands run locally — no SSH.

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
  (project co-members) instead of "everyone with a job on the cluster" (which
  would require polling the whole queue).
- Job ids strip `.gadi-pbs`, nodes strip `gadi-`, queues strip `-exec`;
  suspended (`S`) jobs are shown; `qstat -f` requests are lenient about ids
  that vanish mid-poll; `Resource_List.mem`'s raw-bytes form is handled.

## Uninstall

```bash
cd ~/.local/bin && rm -f monitor qusage qarray
# restore anything that was backed up:
for f in monitor.bak qusage.bak qarray.bak; do [ -e "$f" ] && mv "$f" "${f%.bak}"; done
```

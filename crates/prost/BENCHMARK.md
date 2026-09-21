# Benchmark

prost against the Prophet VS Code extension, measured on 5 September 2026. This is the record
behind the numbers in the README: results, raw samples and how they were taken.

## Setup

| | |
| --- | --- |
| Instance | An on-demand developer sandbox |
| Volume | 57 cartridges, 9.493 files, 631,8 MB |
| Versions | prost 0.1.0 (`--jobs 4`), Prophet 1.4.81 |
| Instrument | Write files under `source/cartridges`, poll the sandbox with `PROPFIND` until they land. Resolution ~0,5 s |
| Isolation | Prophet only writes to `version1`, so prost was pointed at a scratch code version and the poll watched only that one |

Both tools are timed the same way. Prophet stayed enabled during the prost runs, uploading the
same files to `version1` in parallel — a handicap for prost, never an advantage. Running both
against the same code version makes the numbers meaningless: the poll then records whichever
tool arrived first.

## Results

Medians of 5 runs (6 for the save row), with the observed range:

| Scenario | prost | Prophet |
| -------- | ----- | ------- |
| Cold full deploy | 23,7 s | ~60 s |
| Redeploy, nothing changed | 0,4 s | ~60 s |
| 200 files created at once | **2,4 s** (2,3–3,0) | 6,6 s (6,1–7,2) |
| 200 files deleted at once | 3,2 s (1,7–4,3) | **2,2 s** (1,9–6,3) |
| Save to uploaded | 0,89 s (0,86–1,12) | 1,05 s (0,92–1,88) |

The deploy rows are one run each. Prophet's *Clean Project / Upload All* only runs from inside
VS Code, so it was bracketed two ways: 53 s from triggering it to its last archive
disappearing, and 57 s between the first and last cartridge folder timestamp on the sandbox.

## Raw samples

Bulk rows, seconds per run:

| Run | prost create | prost delete | Prophet create | Prophet delete |
| --- | ------------ | ------------ | -------------- | -------------- |
| 1 | 2,6 | 3,2 | 7,2 | 1,9 |
| 2 | 2,3 | 3,2 | 7,0 | 2,0 |
| 3 | 2,3 | 1,7 | 6,6 | 6,3 |
| 4 | 3,0 | 4,3 | 6,3 | 6,3 |
| 5 | 2,4 | 3,6 | 6,1 | 2,2 |

Save to uploaded, milliseconds: prost 1122, 856, 941, 887, 870, 902 · Prophet 985, 984, 1122,
918, 1883, 1112. Prophet's deletions are bimodal — around 2 s or around 6,3 s with nothing in
between, which looks like a second code path rather than network noise.

Through `push` rather than the watcher, on an otherwise untouched code version: cold full
deploy 23,7 s, no-op redeploy 0,4 s, one changed file 1,3 s, 200 changed files 2,4 s, 201
deleted files 5,4 s.

## Reading the results

One cartridge explains most of Prophet's minute. Its unit is the cartridge and it keeps no
state between runs, so every clean deploy ships all 631,8 MB — and the largest cartridge,
268 MB in a single archive, takes 25 of those 60 seconds alone, after the other 56 have
landed. Nothing overlaps inside one archive. prost sends the diff instead, in ~24 MB batches
with four in flight.

Deleting is the row Prophet wins, by a second, and it is the row that matters least: removing
200 files at once is a branch switch, and a branch switch goes through `prost push`, not the
watcher. The second is the watcher's coalescing window — the same window that makes the
creation row 2,4 s instead of 8,7 s.

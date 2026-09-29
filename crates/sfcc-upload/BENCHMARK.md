# Benchmark

sfcc-upload against the Prophet VS Code extension. sfcc-upload was measured on 29 September
2026; Prophet's column is carried over from 5 September, when it was last timed. This is the
record behind the numbers in the README: results, raw samples and how they were taken.

## Setup

| | |
| --- | --- |
| Instance | An on-demand developer sandbox |
| Volume | A full storefront checkout: 56 cartridges, 9 362 files, 629,9 MB |
| Versions | sfcc-upload 0.9.0 (`--jobs 4`); Prophet 1.4.81 on 5 September |
| Instrument | Write files under `cartridges`, poll the sandbox with `PROPFIND` until they land. Resolution is one round trip — polled back to back, not every 0,5 s as in September |
| Isolation | A copy of the checkout outside any other watcher, pushed to a scratch code version created for the run and deleted after it |

Prophet was not re-measured: its *Clean Project / Upload All* only runs from inside VS Code,
and that is not something a script can drive. Its figures below are September's, from the
same instrument at 0,5 s resolution. When it was measured, both tools had to be pointed at
different code versions: against the same one, the poll records whichever tool arrived first.

## Results

Medians of 5 runs (6 for the save row), with the observed range:

| Scenario | sfcc-upload | Prophet (5 Sep) |
| -------- | ----------- | --------------- |
| Cold full deploy | 20,9–35,3 s | ~60 s |
| Redeploy, nothing changed | 0,2 s | ~60 s |
| 200 files created at once | **2,5 s** (2,5–5,3) | 6,6 s (6,1–7,2) |
| 200 files deleted at once | 4,8 s (4,7–5,0) | **2,2 s** (1,9–6,3) |
| Save to uploaded | **0,79 s** (0,74–0,81) | 1,05 s (0,92–1,88) |

The deploy rows are two runs each, 23 minutes apart, with the same volume and the same
command: the spread is the sandbox, not the uploader. Prophet's *Clean Project / Upload All*
was bracketed two ways in September: 53 s from triggering it to its last archive
disappearing, and 57 s between the first and last cartridge folder timestamp on the sandbox.

## Raw samples

Watcher rows, seconds per run:

| Run | create | delete |
| --- | ------ | ------ |
| 1 | 2,47 | 5,04 |
| 2 | 2,66 | 4,87 |
| 3 | 2,54 | 4,77 |
| 4 | 2,51 | 4,82 |
| 5 | 5,29 | 4,72 |

Save to uploaded, milliseconds: 798, 814, 739, 790, 735, 809.

Through `push` rather than the watcher, on an otherwise untouched code version, in two passes:

| | first | second |
| --- | --- | --- |
| Cold full deploy | 20,9 s | 35,3 s |
| No-op redeploy | 0,22 s | 0,22 s |
| One changed file | 0,77 s | 0,88 s |
| 200 changed files | 1,75 s | 1,98 s |
| 201 deleted files | 4,69 s | 4,57 s |

Prophet, 5 September, seconds per run: create 7,2, 7,0, 6,6, 6,3, 6,1 · delete 1,9, 2,0, 6,3,
6,3, 2,2 · save, in milliseconds, 985, 984, 1122, 918, 1883, 1112. Its deletions are
bimodal — around 2 s or around 6,3 s with nothing in between, which looks like a second code
path rather than network noise.

## A deletion the watcher lost

The first pass of this run stopped at the fourth deletion: 139 of the 200 files stayed on the
sandbox while the watcher reported itself in sync. `notify-debouncer-full`, with its file-id
cache, was swallowing deletion events on a checkout this size — reproduced locally with no
sandbox involved, 27 to 72 of 200 deletions coming through in 8 rounds out of 10, with no
error and no rescan. Raw `notify`, and the debouncer without that cache, saw 200 of 200
every time. The watcher now runs without it; every sample above is from the fixed build.

It also means September's 3,2 s deletion row came from a watcher with this bug, and cannot
be compared with today's.

## Reading the results

One cartridge explains most of Prophet's minute. Its unit is the cartridge and it keeps no
state between runs, so every clean deploy ships everything — and the largest cartridge,
268 MB in a single archive, took 25 of those 60 seconds alone, after the other 55 had
landed. Nothing overlaps inside one archive. sfcc-upload sends the diff instead, in ~24 MB
batches with four in flight.

Deleting is the row Prophet wins, by 2,6 s. The watcher sends one `DELETE` per file, four at a
time — the same work `push` does for 201 files in 4,6 s — where Prophet appears to remove
more per request. It is also the row that matters least: removing 200 files at once is a
branch switch, and a branch switch goes through `sfcc-upload push`, not the watcher.

The one slow creation, 5,3 s in run 5, is a single sample; the other four sit within 0,2 s of
each other.

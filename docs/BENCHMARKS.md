# Benchmarks

How the numbers in the README and on [irori.app](https://irori.app) were measured,
so anyone can check them.

## Irori 0.2.0 and Heroic 2.22.4

| | Irori | Heroic |
| --- | --- | --- |
| Memory (median of 3 rounds) | 87.5 MiB | 478.2 MiB |
| Memory (all rounds) | 87.4–92.2 MiB | 472.7–483.4 MiB |
| Processes | 1 | 7 |
| CPU while idle, % of one core | 0.0, 0.0, 0.0 | 6.6, 0.2, 7.1 |
| Program size | 76 MB | 368 MB |

Measured on 30 October 2026 on Nobara (Linux 7.2, KDE on Wayland) with an AMD Radeon
RX 7900 XTX. Irori was the `irori` binary from the 0.2.0 Linux tarball, and Heroic was
Fedora's `heroic-games-launcher` package. Program size is that binary for Irori and
`/usr/lib64/heroic` for Heroic, which includes the store tools Heroic ships with.

## Method

[`benchmarks/compare.py`](benchmarks/compare.py) runs each app with an empty, throwaway
home directory: no logins, no games, and the window open at its default size. It takes
turns between the apps, and in each round it:

1. Starts the app in a new session, so every process it spawns can be found.
2. Waits 45 seconds for startup work to finish.
3. Counts the session's CPU time over the next 30 seconds, as a percentage of one core.
4. Reads memory as the sum of each process's PSS from `/proc/<pid>/smaps_rollup`. PSS
   divides shared pages between the processes using them, so an app with several
   processes isn't counted twice for the same memory.
5. Closes the app.

To repeat it, with both apps installed:

```bash
python3 docs/benchmarks/compare.py --irori irori --heroic heroic
```

It opens and closes windows for about 8 minutes. Your real logins and libraries aren't
used.

## Limits

- **An empty library is the best case for both apps.** A large library with cover art
  will use more, and hasn't been measured yet.
- **Idle CPU comes from fresh profiles.** Heroic's figure varied between rounds and may
  include first-run work, so treat it as a rough reading rather than its usual idle use.
- **Video memory isn't counted** for either app.
- **This is one PC.** Other drivers, compositors and window sizes will give different
  numbers.

<img src="assets/logo.png" alt="" width="104" align="right">

# Yet Another Task Manager

A Windows task manager built for low latency and a small footprint: process
hierarchy, a full details grid, live performance graphs, services and startup
entries. Dark theme, GPU-composited, event-driven.

Measured against the real Windows Task Manager on the same machine while both
sat idle (24 logical cores, 64 GB RAM, ~550 processes, ~9,300 threads):

| | yatm | Windows Task Manager |
|---|---|---|
| CPU at rest | **3.0 %** of one core | 20.6 % of one core |
| Working set | **206 MB** | 238 MB |
| Private bytes | 232 MB | **151 MB** |
| UI frame time | **0.20 – 0.31 ms** (peak 2.6 ms) | n/a |
| Threads | **16** | 24 |
| Growth over 5 min idle | **0** | 0 |

Binary is 10.7 MB with no runtime dependencies beyond what ships with Windows.

Nearly all of the remaining CPU is the cost of *getting* the data, not showing
it. Per one-second tick (`YATM_TRACE_SAMPLE=1`):

```
total 24.1 ms   procs 20.1   net 1.8   gpu 1.3   windows 0.5   mem 0.4
```

The 20 ms is the single `NtQuerySystemInformation` call that returns 554
processes and 9,500 threads — a kernel-side walk of every process and thread, and
the floor for anything that reports this data. Painting the UI costs 0.2–0.3 ms
of that second.

## Why this stack

**Rust** — no GC, so no pause spikes in a program whose whole job is to report
other programs' pauses. Zero-cost FFI to the kernel APIs the data actually comes
from, and a 10 MB self-contained binary.

**egui / eframe (immediate mode)** — input-to-pixel latency is one frame, because
there is no retained widget tree to diff or invalidate. Crucially it is used in
*reactive* mode: the sampler thread calls `Context::request_repaint` after
publishing, so with no input and no new data the UI thread is parked and the
process burns nothing. That is where most of the CPU advantage above comes from.

**wgpu → DirectX 12, not OpenGL** — and this one went the other way on the
second look, so both halves are worth recording.

OpenGL wins on the first measurement and only on the first measurement. A bare
eframe window (`src/bin/memprobe.rs`, which exists to separate framework cost
from ours) starts at 198 MB of private bytes under glow against 446 MB under
wgpu, because wgpu enumerates and initialises every backend it was compiled with
and maps several driver stacks into the process. Restricting it to DX12 on the
low-power adapter and adding `MemoryHints::MemoryUsage` — wgpu otherwise
sub-allocates in large blocks to keep allocation off a hot path this UI does not
have — brings that down to 232 MB.

The reason OpenGL lost is that it does not stay at 198 MB. It grew about
**210 KB per painted frame**, indefinitely: ~400 KB/s with the window merely
open, past 700 MB inside half an hour. The cause is not in this codebase, and it
is not upload volume either — `egui_glow` uploads one vertex buffer per mesh per
frame with `glBufferData(..., STREAM_DRAW)`, which is the textbook approach. The
driver keeps an allocation per distinct *size* passed to `glBufferData`, and a
table of live numbers changes the size of nearly every mesh on every frame.

That last step is the one worth stating precisely, because it is what turns a
suspicion into a diagnosis. The probe was given a few hundred text cells to paint
per frame at 120 fps:

| bare eframe window, glow, 120 fps | growth |
|---|---|
| 400 text cells, count fixed | **0 KB/frame** |
| 400 text cells, count varied per frame | 0.73 KB/frame |

Same pixels, same upload volume, same draw calls — only the buffer sizes differ,
and only the varying case grows. The real app has hundreds of independently
varying meshes, hence the far larger per-frame figure.

So the trade is ~150 MB of extra steady footprint against a leak, and for a
program left open all day that is not a close call. `YATM_RENDERER=glow` selects
OpenGL for a short session or a machine whose DX12 path misbehaves; it is
genuinely lighter for the first ten minutes.

**Raw `NtQuerySystemInformation`** — one syscall returns the entire process table
(PIDs, parents, CPU times, all memory counters, handle/thread counts, I/O
counters, plus every thread's state). This is what Task Manager itself uses. The
alternatives were rejected: WMI means COM and ~100 ms per query; the `sysinfo`
crate opens a handle per process and allocates heavily.

## Architecture

```
sampler thread                          UI thread
──────────────                          ─────────
NtQuerySystemInformation  ──┐
disk IOCTLs, GetIfTable2    │
PDH GPU counters            ├─► Snapshot ──ArcSwap──► load pointer once per frame
EnumWindows                 │   (immutable)           read straight out of it
build tree, roll up subtrees┘        │
                                     └─► request_repaint()  ← wakes the UI

metadata thread                     icon thread
───────────────                     ───────────
version resources                   SHGetFileInfo + GetDIBits
LookupAccountSid                    → 16×16 RGBA → GPU texture
command lines
```

Everything expensive is off the UI thread. Publication is a single atomic
pointer store, so the UI never locks and never sees a half-built snapshot.
Snapshots are recycled through a small pool (reclaimed once no reader holds
them), so the steady state does not reallocate the process table every second.

Three deliberate performance choices:

- **The table paints, it does not instantiate widgets.** A Details view can be
  600 rows × 15 columns; a widget per cell would mean 9,000 `Id` allocations,
  layouts and interaction checks per frame. Instead only rows intersecting the
  viewport are touched, cells are `painter.galley` calls, and the *entire body*
  is one interactive rect whose hovered/clicked row is computed arithmetically
  from the pointer's `y`. Per-frame cost is O(visible rows); hit-testing is O(1).
- **Display lists are cached.** The row list is rebuilt only when something that
  affects it changes (new sample, sort, filter, expand/collapse, freeze).
  Scrolling and hovering cost nothing but painting.
- **Graph fills are meshes, not polygons.** An area under a curve is not convex,
  so `Shape::convex_polygon` fans it from the first vertex and renders visible
  wedges. Emitting one quad per sample interval is correct for any shape and
  cheaper.

Process identity is `(PID, creation time)` everywhere, not bare PID. Windows
recycles PIDs aggressively; without the creation time, selection, expansion
state and rate deltas would silently jump to an unrelated process.

## Tabs

**Processes** — hierarchy in two sections, Apps and Background processes. A whole
process tree is assigned to one section, so a browser's or editor's helper
processes nest under the app rather than scattering into Background. Collapsed
parents report their subtree's totals. CPU / memory / disk cells carry Task
Manager's warm load tint. Elevated processes are marked.

Two details that are deliberate rather than incidental:

- **Grouping follows the sort.** Sorted by name (the default) the list is split
  into Apps and Background processes; sorted by anything else it goes flat.
  Section headers are only useful while the order inside them means something —
  sorted by CPU they cut the ranking in two and hide the busiest background
  process below a fold. There is no button for it because there is nothing to
  decide.
- **Window titles are shown application-first.** Apps title their windows
  document-first and put themselves last (`main.rs - myproject - Visual Studio
  Code`). In a column that is sorted and elided that is backwards: the part you
  scan by is the part that gets truncated. So the trailing segment is promoted,
  and the row reads `Visual Studio Code - main.rs - myproject`.

  Splitting on a separator is not enough to do this safely. There is no
  convention — VS Code uses a hyphen, Firefox an em dash, Teams a pipe — and a
  title that merely *contains* one need not be document-first at all. So the
  trailing segment has to be corroborated as the application before anything
  moves, against either the executable's `FileDescription` or its file name.
  `ms-teams.exe` matches `Microsoft Teams` on the word "Teams" alone. Failing
  both, the title is shown exactly as the app wrote it, which is the safe answer.
  The name rendered is the one from the title rather than from the version
  resource, so it is right on the first frame instead of a second later when the
  metadata thread catches up, and never changes under the reader.

**Performance** — CPU (aggregate or one graph per logical processor), memory with
a composition bar, per-physical-disk active time and transfer rate, per-adapter
network throughput, and GPU utilisation. Graphs keep 60 s of history in
fixed-size ring buffers, and the grid is phase-locked to the sample cadence so
it scrolls with the data.

**Details** — flat grid, 33 columns (21 hidden by default, toggled from the
header's right-click menu): working set, peak, commit, virtual size, paged and
non-paged pool, page faults, handles, threads, base priority, architecture,
session, start time, cumulative and current I/O, CPU time, cycles, publisher,
image path, command line.

**Services** — all Win32 services with state, hosting PID and service group;
right-click jumps to the hosting process.

**Startup apps** — the `Run` keys and both Startup folders, showing where each
entry comes from and whether Windows has it disabled.

## Controls

| | |
|---|---|
| **Ctrl** (held) | freeze the row order — click a row that keeps moving |
| ↑ ↓ | move selection |
| → | expand, or step into the subtree |
| ← | collapse, or step out to the parent |
| Home / End | first / last row |
| Ctrl+F | focus the filter; Esc clears it |
| F5 | sample now (works while paused) |
| Del | end the selected process |
| Double-click a row | expand / collapse |
| Right-click a row | end task, end tree, priority, go to details, open location, properties, copy |
| Right-click a header | choose which columns to show |
| Drag a header edge | resize column; click to sort |
| Click the sidebar icon | collapse the rail to icons only |

Ordering is frozen by *identity*, not row index: processes that appear during a
freeze are appended rather than displacing what is on screen.

Ending a process asks first, and names what it is about to end — including how
many processes an "end tree" covers. Cancel holds the keyboard focus, so Enter
and Escape both do the harmless thing.

The Name column takes whatever width the others leave, so collapsing the rail or
hiding a column widens it rather than leaving a dead strip on the right. Dragging
its right edge resizes the *next* column, because its own width is derived and
widening it would be cancelled out by the slack shrinking. The difference is
signed: when the columns declare more width than there is, that column gives some
back rather than pushing the last heading off the right edge.

The header sits outside the body's scroll area, which means it has to subtract
the scrollbar's width by hand or every column after the stretching one is offset
against its own heading — 14px, and most obvious when collapsing the rail changed
the slack. The bar is therefore always reserved rather than appearing when the
row count crosses the overflow point, since a bar that comes and goes would shift
the whole grid underneath a stationary header.

The window goes down to 500×400. Below about 560px of toolbar the update-speed
caption goes, then the per-tab buttons, then status-bar items drop off one pair at
a time — dropping a whole pair reads as "no room for this", where letting it clip
reads as a rendering fault.

## Saved state

`%APPDATA%\yatm\settings.ini`, flat `key=value` text, written at most once every
two seconds and again on exit:

- window position and inner size (and whether it was maximized)
- whether the navigation rail is collapsed
- per table: which columns are shown, their widths, the sort column and direction

Columns are keyed by a stable id rather than by position, so adding or reordering
columns in a later build leaves an existing file usable. Anything implausible is
ignored rather than applied — a window size below the minimum, or a position from
a monitor that is no longer attached, falls back to the default instead of opening
off-screen. The file is safe to delete; nothing in it is load-bearing.

## Build

```
cargo build --release
./target/release/yatm.exe
```

Requires a Windows SDK (for `ntdll.lib` / `dxgi.lib`) and the MSVC toolchain.
No admin rights needed to run; elevation only widens what is visible.

For development use `cargo build --profile fast`, which is the release profile
without `lto = "fat"` and `codegen-units = 1`. Those two make a *link* of the
whole dependency graph part of every build, on one core: a one-line change costs
**2.4 s** on the `fast` profile against **130 s** on `release`. Neither setting
changes behaviour, so `fast` is what to iterate against and `release` is what to
ship.

Environment variables:

- `YATM_RENDERER=glow` — use OpenGL instead of wgpu/DX12. Lighter to start with,
  but it grows; see the renderer note above.
- `YATM_TRACE_SAMPLE=1` — print per-phase sampler timings to stderr.

`cargo build --features memtrace` adds a counting global allocator and prints
live heap bytes, allocation and frame counts, and the size of every long-lived
container once per sample. It costs two atomics per allocation, which is why it
is a build-time opt-in. It is what proved the growth above was *not* on the Rust
heap — live bytes sat at 10.8 MB while private bytes climbed — and so stopped a
long search through this codebase for a leak that was never in it. The same build
accepts `YATM_UI_LEVEL=0..3` and `YATM_NO_ICONS=1` to bisect the paint path.

### The icon

`assets/yatm.ico` holds nine images: 16, 20, 24, 32, 40, 48 and 64 px as
uncompressed DIBs, 128 and 256 px as PNG. Each one is laid out for its own size
rather than scaled down from a master — below 48 px the geometry is snapped to
the pixel grid so bar edges land on whole pixels, and the grid lines and accent
lift are dropped where they would only add mush. `tools/mklogo.ps1` regenerates
it (Windows PowerShell 5.1, for GDI+):

```
powershell -NoProfile -ExecutionPolicy Bypass -File tools\mklogo.ps1 -Out assets
```

`build.rs` turns that `.ico` into the linker's `.res` input itself. The format is
a flat list of length-prefixed records — one `RT_ICON` per image plus an
`RT_GROUP_ICON` naming them — and MSVC `link` accepts a `.res` directly, so the
build needs neither `rc.exe` nor a build-dependency tree. The same build script
unpacks the 64 px DIB to raw RGBA for the window icon, because winit registers
its window class with a null `hIcon`: without that the caption and Alt-Tab would
fall back to Windows' generic application icon even though Explorer shows the
embedded one. All nine sizes are verified to come back out of the built
executable through `PrivateExtractIcons` at their exact dimensions.

## Limitations

These are real gaps, not oversights — each needs a capability the app does not
have:

- **No per-process network column.** Attributing packets to processes requires an
  ETW consumer and administrator rights. System-wide, per-adapter throughput on
  the Performance tab *is* accurate. Task Manager's per-process figure comes from
  ETW; showing a fabricated number would be worse than omitting the column.
- **No App history tab.** That data lives in the SRUM database, which is not
  usefully readable from a normal process.
- **No startup "impact" rating.** Windows derives it from ETW boot traces. The
  Startup tab reports each entry's source instead.
- **Startup and Services are read-only.** Enabling/disabling startup entries and
  starting/stopping services need elevation and SCM write rights.
- **GPU depends on PDH counters.** `GPU Engine` / `GPU Adapter Memory` are absent
  on some drivers and in many VMs; the GPU cards then simply do not appear.
  Adapter names and VRAM totals are joined by LUID from DXGI.
- **Some rows show "—" without elevation.** Protected processes (anti-malware,
  some system services) refuse `PROCESS_QUERY_LIMITED_INFORMATION`, so path,
  user, publisher and command line are unavailable. Real Task Manager is equally
  blind unelevated.
- **Memory floor is the GPU stack.** ~206 MB working set is mostly the DirectX
  driver and egui's font atlas, not process data — a bare eframe window costs
  most of it. Going meaningfully below that means giving up GPU compositing, and
  the cheaper renderer is the one that leaks.
- **The idle process is not listed.** Its "CPU time" is the machine's idle time,
  which would make the CPU column sum to 100 % forever. Task Manager omits it
  too.

## Layout

```
build.rs              assets/yatm.ico -> linker .res + window-icon RGBA
assets/
  yatm.ico            nine sizes; the executable's icon
  logo.png            512 px, for this README
tools/
  mklogo.ps1          draws the logo and writes both of the above
src/
  main.rs             entry point, renderer selection, restored geometry
  app.rs              shell: nav rail, toolbar, status bar, action dispatch
  settings.rs         %APPDATA% key=value store, hand-parsed
  sys/                data acquisition — knows nothing about the UI
    nt.rs             NtQuerySystemInformation bindings + x64 layout assertions
    sampler.rs        sampler thread, tree building, subtree rollups, grouping
    snapshot.rs       the immutable model handed to the UI
    meta.rs           two-stage per-process metadata (cheap on-thread, slow off)
    counters.rs       CPU / memory / static machine description
    disk.rs net.rs gpu.rs dxgi.rs
    services.rs startup.rs icon.rs actions.rs windows_list.rs util.rs
  ui/
    table.rs          virtualized direct-painting table
    graph.rs          filled line graphs, sparklines, segmented bars
    processes.rs details.rs performance.rs services.rs startup.rs
    theme.rs fmt.rs icons.rs
  memtrace.rs         counting allocator + paint-path bisect (--features memtrace)
  bin/memprobe.rs     bare-window probe used for the renderer measurements above
```

The `nt.rs` structures carry compile-time `offset_of!` assertions for every field
the app reads. A wrong offset would not crash — it would display plausible
garbage — so the documented x64 layout is pinned by the build.

<img src="assets/app-256.png" alt="" width="104" align="right">

# Azur Task Manager

A Windows task manager built for low latency and a small footprint: process
hierarchy, a full details grid, live performance graphs, services and startup
entries. GPU-composited, event-driven, and named for the design system it is
built out of — Azur (`azur-egui-theme`, a sibling checkout) down to the title
bar, the icon and the colour behind a busy cell.

Measured against the real Windows Task Manager on the same machine while both
sat idle (24 logical cores, 64 GB RAM, ~550 processes, ~9,300 threads):

| | Azur | Windows Task Manager |
|---|---|---|
| CPU at rest | **3.0 %** of one core | 20.6 % of one core |
| Working set | **206 MB** | 238 MB |
| Private bytes | 232 MB | **151 MB** |
| UI frame time | **0.20 – 0.31 ms** (peak 2.6 ms) | n/a |
| Threads | **16** | 24 |
| Growth over 5 min idle | **0** | 0 |

Binary is 10.7 MB with no runtime dependencies beyond what ships with Windows.

Nearly all of the remaining CPU is the cost of *getting* the data, not showing
it. Per one-second tick (`AZTM_TRACE_SAMPLE=1`):

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
program left open all day that is not a close call.

**And the loser is not shipped.** OpenGL stayed selectable behind
`AZTM_RENDERER=glow` for a while after losing — genuinely lighter for the first ten
minutes, and insurance for a machine where wgpu will not start. That cost 205,824
bytes of every copy of the executable, measured, and the name went with the code
rather than being left as an override this build can no longer honour. The table
above is the record of why; re-running the comparison means adding eframe's `glow`
feature back for as long as it takes to measure.

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

**Processes** — hierarchy in three sections: Apps, Background processes, and
Suspended processes. A whole process tree is assigned to one section, so a browser's
or editor's helper processes nest under the app rather than scattering into
Background. Collapsed parents report their subtree's totals. Elevated processes are
marked.

Suspended is last, and being frozen outranks being an app. A suspended process
consumes nothing and can do nothing, so a reader scanning for what is using the
machine wants the whole lot of them out of the way at once rather than interleaved
with the things that are running.

CPU / memory / disk cells carry a magnitude tint in **that column's own series
colour** — the violet of the memory trace behind a heavy memory cell, the green of
the disk trace behind a heavy disk cell. Task Manager's scheme, which this used to
copy, ramps amber into red for every column alike: it says *alarm* about a process
doing nothing wrong, and it says the same thing about memory as about disk. Severity
is the number's job, not the cell's. Nothing is tinted below 12% of full scale
either, because a list where almost everything is coloured tells a reader nothing —
the point is that the eye lands on the few rows that are actually busy.

Two details that are deliberate rather than incidental:

- **Grouping follows the sort.** Sorted by name (the default) the list is split
  into its sections; sorted by anything else it goes flat.
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

**Sensors** — 60 seconds of what the machine reports about itself, a trace per
reading: CPU usage, clock, package temperature and package power; memory in use and
committed; per-GPU usage, video memory, temperature, hotspot, fan, core clock and
power; and every ACPI thermal zone the firmware publishes. Traces rather than dials,
because
a sensor's value only means something next to where it has just been — a package
at 60 °C reads one way after a minute of idling and another thirty seconds into a
build. None of it needs elevation. Temperatures come from the
`Thermal Zone Information` counter set, the live CPU clock from
`CallNtPowerInformation`, and each GPU's own readings from its vendor's library,
loaded at run time and absent without that vendor's driver: **NVML** for NVIDIA,
**Level Zero Sysman** for Intel, **ADL** for AMD. Each adapter is matched to a
backend by PCI vendor id and position, never by name. A reading the machine does
CPU package power comes from `\Energy Meter(*)\Power`, which is the Energy Metering
Interface — on an Intel part that surfaces the RAPL rails, the same registers a
power-monitoring tool reads through a kernel driver, published by Windows and readable
by anyone. The package rail is the one reported: it is what the part's power limit
applies to, and it is what "CPU power" means. On an AMD APU the same figure comes from
ADL instead, along with the processor's temperature, because there the CPU and the
graphics are one die and one sensor set covers both.

A reading the machine does not have shows as "—" rather than as a dial at zero, and an
adapter with no
readings at all says which library it wanted and — when that library is installed
and still answered nothing — that this is what happened, because "no such card"
and "the interface is there and mute" call for different fixes.

The *hotspot* is the exception to that rule: it appears only on hardware that
publishes one, rather than as an empty card everywhere else. Temperature and fan are
things every discrete board has and an integrated one honestly lacks, so "—" says
something true about the hardware; a junction reading is an AMD feature, and an empty
card would say something false.

**Frame pacing** for whatever is in the foreground is the last section: FPS, mean frame
time, the 99th-percentile FPS (the "1% low" — the rate the slowest one frame in a
hundred arrives at) and a micro-stutter rate. The four go together on purpose: an
average of 120 fps says nothing about whether the frames arrived evenly, the 1% low
says whether the worst of them were bad, and the stutter rate catches judder that
neither notices — alternating 8 ms and 16 ms intervals average out to a healthy-looking
83 fps and look terrible.

This one needs **administrator rights**, and that is a platform limit rather than a
choice. A frame is a *present*, presents are published by the graphics providers, and
reading another process's events means an ETW trace session — which Windows grants only
to a caller holding `SeSystemProfilePrivilege`. There is no unelevated substitute: the
performance counters publish GPU engine time and memory, never frames. Unelevated the
section says so and shows four empty recesses rather than four zeros.

Micro-stutter has no standard definition, so this one states its own: the share of
frames whose interval differs from the one before it by more than a fifth of the
window's median. A proportion rather than a fixed number of milliseconds, because
"visibly late" scales with the frame rate — 2 ms is nothing at 30 Hz and a third of a
frame at 144.

The page is the reader's to arrange, and it is the one page in the app that is:
drag a heading to move a section, drag a card to move it within its section,
right-click for the list of what is switched on. A table's rows are all the same kind
of thing, so sorting them says everything there is to say — a sensor page is a wall
of unrelated numbers of which any given reader cares about four, and whose four is
not something the app can know. It is all kept in `settings.ini`, keyed by *what each
thing is* rather than by where it sits, so an arrangement survives a driver update, a
second graphics card, or a firmware that renumbers its thermal zones. Anything the
saved arrangement has never seen appears at the end of its group rather than not at
all. `Reset layout`, in the same menu, puts everything back in the order the machine
reports it.

**Details** — flat grid, 33 columns (21 hidden by default, toggled from the
header's right-click menu): working set, peak, commit, virtual size, paged and
non-paged pool, page faults, handles, threads, base priority, architecture,
session, start time, cumulative and current I/O, CPU time, cycles, publisher,
image path, command line.

**Services** — all Win32 services with state, hosting PID and service group;
right-click jumps to the hosting process or opens the folder its image lives in.

**Startup apps** — the `Run` keys and both Startup folders, showing where each
entry comes from and whether Windows has it disabled. Enable and disable write
Explorer's own `StartupApproved` record, so nothing is deleted and the change is
what Windows itself would have made; entries that apply to all users need
administrator rights, and the error says so. Right-click also opens the folder the
entry runs from — a `Run` value points at a program, a Startup-folder entry *is* a
shortcut, so that one lands on the Startup folder with the shortcut selected.

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
| Right-click a row | end task, end tree, go to details, open folder, properties, copy, and priority in a submenu |
| Hover a row | every column of it, uncropped, in a 400px tooltip at the pointer — names in one measured column, each value on its label's baseline |
| Right-click a header | choose which columns to show |
| Drag a header edge | resize column; click to sort |
| Drag a sensor card's title | move it within its section — the trace is not a handle |
| Drag a sensor heading | move the whole section |
| Right-click in Sensors | choose which sensors and sections to show; reset the layout |
| Run as administrator | adds frame pacing for the foreground app; nothing else needs it |
| Click the sidebar icon | collapse the rail to icons only |

Ordering is frozen by *identity*, not row index: processes that appear during a
freeze are appended rather than displacing what is on screen.

Ending a process asks first, and names what it is about to end — including how
many processes an "end tree" covers. The button that goes through with it holds
the keyboard focus, so Enter confirms and Escape backs out; the dialog was opened
by asking for the action, and the way out is the key that means "no".

**New task**, beside End task on the two tabs that list processes, opens Windows'
own Run dialog — `RunFileDlg`, on a thread of its own so a modal dialog cannot
freeze this window, and with no owner so it stands free exactly as Win+R gives it.

It opens *centred under the button*, `space-2` below it, which takes three things the
API does not offer. `RunFileDlg` has no position argument, so the placement goes
through a `WH_CBT` hook on the dialog's own thread: `HCBT_ACTIVATE` is the moment the
window exists and is sized but is not yet on screen, so moving it there is a
placement and not a jump across the desktop. The position has to be in *screen*
pixels, which egui never deals in — the conversion is one multiplication because a
widget rectangle and the viewport's `inner_rect` are in the same points, and
egui-winit divides the physical rectangle by exactly the `pixels_per_point` used to
multiply it back, so neither a display scale nor a zoom factor can make the two
disagree. And what the toolbar hands over is the button's *centre*, not a corner:
`RunFileDlg` sizes its own window, so the only place that knows how wide the dialog
is — and can subtract half of it — is inside the hook. The result is clamped to the
work area of the monitor the anchor is on: the button sitting near an edge of a screen
would otherwise put the dialog's own buttons under the taskbar or past the side.

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

The window goes down to 500×400. The navigation rail collapses to 48px of icons,
the filter box shrinks to 120px before anything else gives, the update-speed
caption goes, and status-bar items then drop off one pair at a time — dropping a
whole pair reads as "no room for this", where letting it clip reads as a rendering
fault.

## Saved state

`%APPDATA%\Azur Task Manager\settings.ini`, flat `key=value` text, written at
most once every two seconds and again on exit:

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
./target/release/azur-task-manager.exe
cargo deploy                    release build, then copy it to D:\Programs
cargo deploy --profile fast     the iteration build, for once
```

Requires a Windows SDK (for `ntdll.lib` / `dxgi.lib`) and the MSVC toolchain.
No admin rights needed to run; elevation only widens what is visible.

`cargo run` runs the application: the crate sets `default-run`, because the second
binary — `memprobe`, below — would otherwise make cargo refuse to choose. That one
is a measurement tool rather than part of the product, so it is behind a feature of
its own and `cargo build` does not build it. Ask for it by name when it is needed:

```
cargo run --features memprobe --bin memprobe
```

For development use `cargo build --profile fast`, which is the release profile
without `lto = "fat"` and `codegen-units = 1`. Those two make a *link* of the
whole dependency graph part of every build, on one core: a one-line change costs
**2.4 s** on the `fast` profile against **130 s** on `release`. Neither setting
changes behaviour, so `fast` is what to iterate against and `release` is what to
ship.

`cargo deploy` builds and then copies the binary to `D:\Programs`, or to
`$AZUR_DEPLOY_DIR`, or to a directory named on the command line. It is an alias for
`run --release --example deploy`, because nothing in cargo will build and *then*
copy — the work is [`azur_egui_theme::deploy`](../azur-egui-theme/src/deploy.rs),
shared with the other Azur applications, and `examples/deploy.rs` here is one
call. It copies nothing if the build failed, and it can replace a copy that is
*running*: this is a window you leave open, so overwriting it would otherwise fail
for sharing every time. `--profile fast` deploys the iteration build, which is worth
knowing here specifically — those 130 seconds against 2.4 are the difference between
watching a deploy and not.

`--bin` in the helper is what keeps `memprobe` out of it: a deploy asks for the
application by name rather than for whatever the profile directory happens to hold.

Environment variables:

- `AZTM_TRACE_SAMPLE=1` — print per-phase sampler timings to stderr.
- `AZUR_BACKEND`, `AZUR_ADAPTER=low` — the design system's, and the only renderer
  knobs there are; there is no `AZTM_RENDERER` any more (see the renderer note
  above).

`cargo build --features memtrace` adds a counting global allocator and prints
live heap bytes, allocation and frame counts, and the size of every long-lived
container once per sample. It costs two atomics per allocation, which is why it
is a build-time opt-in. It is what proved the growth above was *not* on the Rust
heap — live bytes sat at 10.8 MB while private bytes climbed — and so stopped a
long search through this codebase for a leak that was never in it. The same build
accepts `AZTM_UI_LEVEL=0..3` and `AZTM_NO_ICONS=1` to bisect the paint path.

### Binary size

```
11,832,832 bytes  ->  8,593,920 bytes      -27.4%
```

Nothing the program does was given up except the OpenGL renderer, which is priced
above. Every number here came out of a linked executable; where a linker map
disagreed, the map is the one that was wrong.

| Measured | Decision |
| ---: | --- |
| −1,418,240 | **Drop `eframe/default_fonts`.** epaint's four TrueType files were the four largest objects in the binary and **no code path could reach any of them**: `azur-egui-theme::fonts` installs from `FontDefinitions::empty()`. Free, and nothing changes on screen. |
| −1,059,840 | **Take eframe's `wgpu_no_default_features`, not `wgpu`.** Its `wgpu` feature re-enables `egui-wgpu/default` → `wgpu/default` → every backend wgpu has, on top of the one the theme pins. Free. |
| −555,008 | **`opt-level = "z"` on `naga`, `codespan-reporting`, `hexf-parse`.** A shader is translated once, while the window is hidden. Costs ~30 s of release build time. |
| −205,824 | **Drop `eframe/glow`.** A second renderer that lost its measurement; `AZTM_RENDERER` went with the code. |
| −1,024 | **`opt-level = "z"` on `image` and `png`** — attempted, reverted. Same run-once argument as `naga`, and worth a kilobyte: their code is monomorphised into the caller's codegen unit, where a per-package profile does not reach it. |
| — | **`panic = "abort"`** was already on, and `grep -rn catch_unwind` finds nothing, so it stays available. It is the largest single profile lever in a Rust GUI (the file explorer measured 3,570,688 and could not take it); here it was taken before this audit started, which is why `.pdata` is 0.9% of the file rather than 1.9%. |

The two findings worth carrying to the next program: **the fonts were dead weight
that the theme's own manifest predicted would come back silently**, and it came back
through this application's feature list, exactly as written there. And **a map prices
the code that is there, while only a build shows what leaves with a feature** — the
map attributed 78,272 bytes to the dx12 and gles crates, and removing them was worth
1,059,840, because naga's HLSL and GLSL writers, `range-alloc`, gpu-allocator's D3D12
path and most of the `windows` crate went with them.

What is left, by defining crate, from a map of the shipped binary:

| ≈ Bytes | What | Why it stays |
| ---: | --- | --- |
| 2.25 MB | `core`, `std`, `alloc`, `hashbrown` | this is what `panic = "abort"` already attacks |
| 1.66 MB | `wgpu-core` (523 KB of validation), `wgpu-hal`'s Vulkan backend, `wgpu-types`, `egui-wgpu`, `ash` | no feature knobs remain |
| 1.56 MB | `harfrust`, `skrifa`, `read-fonts`, `vello_cpu`, `fearless_simd` | **non-optional** epaint 0.35 dependencies: two CPU rasterisers and a shaper, whether or not a program draws through the GPU |
| 633 KB | `naga` | already at `"z"`; below this is hand-writing a Vulkan painter |
| 695 KB | this application and its theme, 30k lines | reducible only by de-monomorphising UI closures |
| 852 KB | `egui`, `epaint`, `winit`, `eframe`, `egui-winit` | the framework |
| 112 KB | `.rsrc` — the icon, nine PNG-compressed sizes | 1.3% of the file, and 69 KB of it is the 256px entry Explorer's largest view reads |

`opt-level = "s"` for the whole graph is the one large lever left and is **not taken**:
this is the program that prints its own frame cost in the status bar, and the file
explorer's audit measured a comparable graph at −2,960,896 bytes for a slower frame
path. That number is quoted from that audit rather than linked here, which is the
only figure on this page that was not.

### The mark

A rounded near-black plate carrying a blue window, a white trace across it and a
blue gear over the corner — a rendered image rather than a drawing on a grid, kept
in the repository at `assets/app-source.png` so everything below can be derived
again.

The plate is `#1A1C21` at the top and darkens toward the bottom, a shade off the
`GRAY_2` (`#14171A`) the window's own chrome is painted in, so the icon is in the
same paint as the thing it opens. That also means it is barely 1.1:1 against
Windows 11's dark taskbar: the tile disappears there and the icon reads as a blue
window floating on the bar, the way a good many Windows 11 icons do. On a light
taskbar it is a deliberate dark tile.

Everything shipped is reduced from that one image, which arrives 1254 × 1254 with no
alpha at all. Two things about getting from there to nine icons are worth writing
down, because both were wrong first:

- **The transparency is flood-filled, not thresholded.** The surround is opaque
  black and the plate's own interior is nearly as dark, so a luminance cut takes
  holes out of the middle of the artwork. Filling inward from the border instead
  stops at the plate's lit rim and leaves everything it encloses alone. The crop is
  the filled shape's bounding box — 1043 × 1057, the plate being a shade taller than
  it is wide — padded to square rather than squashed to it.
- **Nothing premultiplies the colour by hand.** Pillow already resamples RGBA
  premultiplied, which is why the transparent black does not bleed into the plate's
  edge on the way down. Doing it by hand as well and dividing back out afterwards
  is what put white specks along the corner arcs: at an edge alpha of 10, dividing a
  legitimately dark pixel by 10/255 lifts it to 255. Lanczos then leaves a few
  pixels 1/255 opaque outside the arcs, where the filter's negative lobes undershoot,
  and those are dropped to fully transparent.

It exists in three forms, which is what `docs/icons.md` §12 asks for:

| where | what it is | who supplies it |
| --- | --- | --- |
| Explorer, a pinned shortcut, Alt-Tab before launch | `assets/app.ico`, nine sizes, in the PE resources | `build.rs` |
| the taskbar button of a running window | `assets/app-64.png`, decoded at startup | `ViewportBuilder::with_icon` |
| this window's own title bar, a 16-point box | `assets/app-16.png` / `-32` / `-64` as a texture | `TitleBar::icon`, via `ui::app_mark` |

Miss the second and the taskbar button turns into egui's white `e` the moment the
window opens. The third is where a raster mark costs something a vector one did
not: a glyph painted from geometry is right at every size, and an image is right at
exactly the size it was reduced to. `TitleBar` draws the app icon in a 16-*point*
box, which is 16 physical pixels on a 1× display and 32 on a 2× one, so `app_mark`
uploads the nearest source at or above what the display asks for and swaps it if
that changes. 16 and 32 are the two that land 1:1; the 64 is there so a 250 %-scaled
display reduces a larger image instead of magnifying a smaller one.

`build.rs` turns the `.ico` into the linker's `.res` input itself. The format is a
flat list of length-prefixed records — one `RT_ICON` per image plus an
`RT_GROUP_ICON` naming them — and MSVC `link` accepts a `.res` directly, so the
build needs neither `rc.exe` nor a build-dependency tree. All nine sizes are
verified to come back out of the built executable through `PrivateExtractIcons`
at their exact dimensions, which is worth checking rather than assuming here:
every entry in the `.ico` is PNG-compressed, and PNG icon entries are a Vista-era
addition that the older shell paths predate.

That is also why the window icon is a `.png` and not unpacked from the `.ico`.
The build script has no PNG decoder — deliberately, it is 200 lines of byte
layout with no dependencies — so it cannot read those entries; `eframe::icon_data`
decodes the same 64 px image at startup instead, one indirection shorter. 64 of
the nine because winit builds a single `HICON` and lets Windows scale it, and 64
halves exactly to the taskbar's 32 and quarters exactly to the caption's 16. What
survives down there is a plate, a blue window and one bright trace across it, and
those two steps are clean box reductions — so an exact factor is worth more than a
bigger source.

It writes one more record: an `RT_VERSION` carrying `FileDescription`,
`ProductName`, `CompanyName` and the version numbers. That is the other half of
having a name on Windows — it is what Explorer's Description column, the shell
property sheet and the real Task Manager's Name column read, and it is what
`sys::meta` reads out of *every* executable to fill this program's own Name and
Publisher columns. Without it this program was the one row in its own list that
could not say what it was.

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
- **Services are read-only.** Starting and stopping them needs SCM write rights,
  which means elevation.
- **Startup entries take effect at the next sign-in.** Explorer reads
  `StartupApproved` when it starts, and there is no way to ask it to re-read.
  All-users entries live under `HKEY_LOCAL_MACHINE` and need elevation to change.
- **Temperature and fan depend on the hardware.** ACPI thermal zones are whatever
  the firmware chose to publish, under names only its vendor documents. A GPU's
  temperature and fan come from its vendor's library, and an *integrated* GPU has
  neither of its own: it shares the CPU package's temperature, and its fan belongs
  to the embedded controller. Level Zero on an Intel iGPU therefore reports the
  core clock and little else, which is what the tab shows.
- **The AMD backend was written without a Radeon to try it on**, and it showed:
  ADL reports `iVendorID` as the *decimal* number 1002, not the `0x1002` that PCI and
  the rest of this app use, so the vendor filter matched nothing and every AMD machine
  looked like a machine with no AMD card in it. Both encodings are accepted now, a
  test pins it, and the adapter walk is exercised against a stand-in for the driver.
  It has since run on real hardware. Structure sizes are pinned by `const` assertions
  and every reading is range-checked before it is accepted, but which of the
  individual PMLog sensors are *right* on which generation is still something only a
  Radeon owner can confirm.
- **Which event is a present was taken on trust, and has now been checked.**
  `src/sys/frames.rs` keys on `Microsoft-Windows-DXGI` id 42 and
  `Microsoft-Windows-DxgKrnl` id 184, which come from the providers' manifests rather
  than from any header. `probes_the_frame_providers`, run elevated against a 144 Hz
  display, resolved them through TDH and found exactly that: DXGI 42 is `Present/Start`
  and DxgKrnl 184 is `Present/Info`, both reporting the same process at 141.6 events a
  second out of the 82 event kinds the two providers emit between them. Nothing above
  is a guess any more.

  The redundancy stays, because a manifest is a Windows build's business and not ours:
  both sources are counted per process and whichever saw more of that process's frames
  is the one reported, so an id that changes meaning costs the other source's numbers
  rather than all of them. To re-check on another machine:

  ```
  set AZTM_PROBE_SECS=8
  cargo test --bin azur-task-manager probes_the_frame_providers -- --ignored --nocapture
  ```

  It prints every event kind the providers emit with the rate it implies for the
  busiest process; the present is the one that matches the display against a pid you
  recognise. It needs administrator rights, because so does the feature.
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
build.rs              assets/app.ico -> linker .res (icon + version strings)
assets/                 every file below reduced from app-source.png
  app-source.png      the artwork as rendered, 1254px, no alpha
  app.ico             nine sizes; the executable's icon
  app-16.png          the title bar at 1x, app-32.png at 2x
  app-64.png          the running window's icon, and the title bar past 2x
  app-256.png         for this README
src/
  main.rs             entry point, renderer selection, restored geometry
  app.rs              shell: Azur title bar, rail, command row, status bar, dispatch
  settings.rs         %APPDATA% key=value store, hand-parsed
  sys/                data acquisition — knows nothing about the UI
    nt.rs             NtQuerySystemInformation bindings + x64 layout assertions
    sampler.rs        sampler thread, tree building, subtree rollups, grouping
    snapshot.rs       the immutable model handed to the UI
    meta.rs           two-stage per-process metadata (cheap on-thread, slow off)
    counters.rs       CPU / memory / static machine description
    disk.rs net.rs gpu.rs dxgi.rs
    pdh.rs            the little of PDH the counters need, English paths
    sensors.rs        thermal zones, CPU clock and power, and the GPU backends
    nvml.rs           NVIDIA, via nvml.dll
    level_zero.rs     Intel, via ze_loader.dll (Level Zero Sysman)
    adl.rs            AMD, via atiadlxx.dll — see Limitations
    etw.rs            a real-time trace session and its consumer (needs elevation)
    frames.rs         the foreground app's frame pacing, from present events
    services.rs startup.rs icon.rs actions.rs windows_list.rs util.rs
  ui/
    table.rs          virtualized direct-painting table
    graph.rs          filled line graphs, sparklines, segmented bars
    processes.rs details.rs performance.rs sensors.rs services.rs startup.rs
    theme.rs          the app's slice of the Azur theme: series colours, thresholds
    app_mark.rs       the product mark in the title bar, as a texture
    fmt.rs icons.rs
  memtrace.rs         counting allocator + paint-path bisect (--features memtrace)
  bin/memprobe.rs     bare-window probe used for the renderer measurements above
                      (--features memprobe; not built by a plain `cargo build`)
```

The `nt.rs` structures carry compile-time `offset_of!` assertions for every field
the app reads. A wrong offset would not crash — it would display plausible
garbage — so the documented x64 layout is pinned by the build.

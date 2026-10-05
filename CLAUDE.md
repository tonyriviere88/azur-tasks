# CLAUDE.md

Operating notes for an agent working in this repository. Facts and rules only — the
reasoning behind most of it is in the `//!` module headers and the comments in
`Cargo.toml` and `.cargo/config.toml`. Read the header of a module before changing it.

`azur-tasks` ("Azur Tasks") — a Windows task manager in Rust on egui 0.35, ~24k lines across
~49 files in `src/`. Windows-only. Git dependency (pinned by `Cargo.lock`):
`azur-egui-theme`, the shared Azur design system (title bar, tokens, components, renderer configuration, `deploy`).
The product name lives in one constant, `PRODUCT_NAME` in [main.rs](src/main.rs); `build.rs`
repeats it for the version resource.

---

## Build

**Always build into `target/claude`, via the aliases in `.cargo/config.toml`:**

```powershell
cargo claude-build     # -> target/claude/azur-tasks.exe
cargo claude-clippy    # all targets, -D warnings on purpose
cargo claude-test
cargo claude-doc       # checks the intra-doc links this codebase leans on
cargo claude-run       # see "Running the app" below first
```

- `target/debug` and `target/release` belong to the user. Sharing them means cargo's
  fingerprints and Windows file locks collide with rust-analyzer ("did not finalize
  incremental compilation session directory: Access denied").
- `[profile.claude]` inherits `dev` and changes nothing else, on purpose — what an agent
  builds must be what the developer builds. Do not add flags to it.
  `[profile.dev.package."*"]` gives dependencies `opt-level = 3`; a debug wgpu is unusable.
- `--profile fast` (release without fat LTO / single codegen unit: 2.4 s vs 130 s per
  one-line change) is **for performance measurement only**. `--release` is for publication.
- `cargo deploy` builds release and copies to `D:\Programs` (or `$AZUR_DEPLOY_DIR`, or a
  directory argument); it can replace a running copy. It is `run --release --example deploy`
  over `azur_egui_theme::deploy`. `cargo deploy --profile fast` deploys the iteration build.
- `default-run = "azur-tasks"`. The second binary, `memprobe`, is behind
  `required-features = ["memprobe"]`: `cargo run --features memprobe --bin memprobe`.
- Requires a Windows SDK (`ntdll.lib`, `dxgi.lib`) and the MSVC toolchain.

## Running the app

**This program has no command line and no `--profile` flag.** `cargo claude-run` reads and
writes the user's real `%APPDATA%\Azur Tasks\settings.ini` — window geometry, rail state,
per-table columns, widths, sort, and the Sensors layout. Resizing the window or dragging a
column to check a change overwrites their layout. Say so before running it, or offer to add a
`--profile <name>` flag mirroring Azur Audio Switcher's.

- `#![cfg_attr(not(test), windows_subsystem = "windows")]` in every profile.
  `sys::util::attach_parent_console` borrows the launching shell's console, so stderr output
  still reaches a terminal, but the prompt returns first and output lands under it.
- The guard is `not(test)` because a `windows`-subsystem test binary has no std handles and
  std reports writes to them as *successful* — the `sys` probes would print nothing and pass.
- No admin rights needed. Elevation adds frame pacing (ETW) and fills rows protected
  processes otherwise hide.

Environment variables:

| var | effect |
| --- | --- |
| `AZT_TRACE_SAMPLE=1` | per-phase sampler timings to stderr |
| `AZUR_ADAPTER=low\|high`, `AZUR_BACKEND` | the theme's renderer knobs, the only ones there are |
| `AZT_UI_LEVEL=0..3`, `AZT_NO_ICONS=1` | bisect the paint path (`--features memtrace` builds) |
| `AZT_PROBE_SECS`, `AZT_PROBE_*` | machine-report probes and `memprobe` |

`--features memtrace` adds a counting global allocator (two atomics per allocation) that
prints live heap bytes and long-lived container sizes each tick.

---

## Tests

~93 unit tests in `#[cfg(test)]` modules beside the code. A few are `#[ignore]`d machine
probes that print what the hardware reports.

```powershell
cargo claude-test <filter>
cargo claude-test <filter> -- --ignored --nocapture
```

- **Run only the tests the change touched**, and check a failure against `HEAD` before
  assuming you caused it.
- `probes_the_frame_providers` needs administrator rights and a foreground app presenting
  frames: `AZT_PROBE_SECS=8`, then
  `cargo claude-test --bin azur-tasks probes_the_frame_providers -- --ignored --nocapture`.
- Tests must not write the user's settings file.

---

## Architecture

```
sampler thread                          UI thread
NtQuerySystemInformation  ──┐
disk IOCTLs, GetIfTable2    │
PDH GPU counters            ├─► Snapshot ──ArcSwap──► load pointer once per frame
EnumWindows                 │   (immutable)
build tree, roll up subtrees┘        └─► request_repaint()  ← wakes the UI

metadata thread: version resources, LookupAccountSid, command lines
icon thread:     SHGetFileInfo + GetDIBits → 16×16 RGBA → GPU texture
inspect worker:  modules / handles / threads of one process, on demand
```

| module | responsibility |
| --- | --- |
| [`sys/`](src/sys/) | data acquisition — knows nothing about the UI |
| [`sys/nt.rs`](src/sys/nt.rs) | `NtQuerySystemInformation` bindings, `offset_of!` assertions per field read |
| [`sys/sampler.rs`](src/sys/sampler.rs) | sampler thread, tree building, subtree rollups, section grouping |
| [`sys/snapshot.rs`](src/sys/snapshot.rs) | the immutable model handed to the UI, pooled |
| [`sys/meta.rs`](src/sys/meta.rs) | two-stage per-process metadata (cheap on-thread, slow off) |
| [`sys/inspect.rs`](src/sys/inspect.rs) | one process's modules, handles, threads; newest request wins |
| [`sys/sensors.rs`](src/sys/sensors.rs), [`gpu_backend.rs`](src/sys/gpu_backend.rs) | thermal zones, CPU clock/power, vendor GPU libraries (`nvml`, `level_zero`, `adl`) |
| [`sys/etw.rs`](src/sys/etw.rs), [`frames.rs`](src/sys/frames.rs) | real-time trace session, foreground frame pacing (elevated) |
| [`app/`](src/app/) | the `App`, tabs, chrome (title bar, rail, status bar), performing an `Action` |
| [`ui/table.rs`](src/ui/table.rs) | virtualized direct-painting table |
| [`ui/list.rs`](src/ui/list.rs) | lifecycle shared by the four table-backed tabs |
| [`ui/inspector.rs`](src/ui/inspector.rs) | panel under the list for the selected process |
| [`ui/theme.rs`](src/ui/theme.rs) | the app's slice of Azur: series colours, thresholds |
| [`settings.rs`](src/settings.rs) | hand-parsed `key=value` store |

Invariants. Breaking any of them compiles and then misbehaves at runtime:

1. **Nothing expensive on the UI thread.** Publication is one atomic pointer store; the UI
   never locks and never sees a half-built snapshot. Snapshots are recycled through a pool.
2. **Reactive repaint.** The UI thread is parked unless input arrives or the sampler calls
   `request_repaint`. Do not add continuous repaints; idle CPU is the product's main feature.
3. **The table paints, it does not instantiate widgets.** Only visible rows are touched, cells
   are `painter.galley` calls, and the whole body is one interactive rect with the hovered row
   computed from pointer `y`. Per-frame cost O(visible rows), hit-testing O(1).
4. **Display lists are cached**, rebuilt only on new sample, sort, filter, expand/collapse or
   freeze. Scrolling and hovering cost only painting.
5. **Process identity is `(PID, creation time)`**, never bare PID — Windows recycles PIDs, and
   selection, expansion and rate deltas would jump to an unrelated process.
6. **`nt.rs` layouts are pinned by `offset_of!` assertions.** A wrong offset displays plausible
   garbage rather than crashing. Add an assertion for every new field read.
7. **The Summary page measures nothing of its own.** Every figure is one another tab shows.
   Its rankings are subtree totals over section-heading rows — the same partition as a folded
   Processes tab, so each process is counted once.

---

## Do not "fix" these

Each looks like a bug or an oversight and is a measured or deliberate decision:

- **The renderer is wgpu only, configured by `azur_egui_theme::render::wgpu_options()`**
  (Vulkan, one backend, `MemoryHints::MemoryUsage`). This program does not choose the backend.
  Vulkan measured ~105 MB lighter than DX12 on the same adapter. The discrete adapter is the
  default deliberately; `AZUR_ADAPTER=low` saves ~131 MB where cross-adapter presentation has
  been checked.
- **There is no OpenGL renderer and no `AZTM_RENDERER`.** glow started lighter but grew
  ~210 KB per painted frame, indefinitely: the driver keeps an allocation per distinct
  `glBufferData` size, and a table of live numbers varies nearly every mesh size every frame.
  Proven with `memprobe`, 400 text cells at 120 fps: fixed count 0 KB/frame, varied count
  0.73 KB/frame. `memtrace` showed the Rust heap flat at 10.8 MB meanwhile — the leak was never
  in this code. Removing glow saved 205,824 bytes. To re-measure, add eframe's `glow` feature
  back temporarily. (`memprobe.rs`, `main.rs` and `Cargo.toml` comments point to "the README"
  for these figures; they are here now.)
- **Raw `NtQuerySystemInformation`, not WMI or `sysinfo`.** One syscall returns every process
  and thread (~20 ms for ~550 processes / ~9,500 threads). That is the floor for the sampler's
  ~24 ms tick; painting costs 0.2–0.3 ms. WMI is COM at ~100 ms a query; `sysinfo` opens a
  handle per process.
- **Cargo features are there for binary size** (11.8 MB → 8.6 MB). `wgpu_no_default_features`,
  never eframe's `wgpu` (re-enables every backend: 1,059,840 bytes). No `default_fonts` (the
  theme installs from `FontDefinitions::empty()`: 1,418,240 bytes). `opt-level = "z"` only on
  run-once crates (`naga`, `codespan-reporting`, `hexf-parse`); it does nothing for small
  generic crates like `image`/`png` (1 KB). `opt-level = "s"` globally is rejected for frame
  cost. Measure a linked binary; a linker map under-prices what leaves with a feature.
- **`panic = "abort"`** — nothing uses `catch_unwind`. Keep it that way.
- **Graph fills are meshes, one quad per sample interval**, not `Shape::convex_polygon`, which
  fans a non-convex area into visible wedges.
- **Magnitude tints use the column's own series colour**, not an amber→red alarm ramp, and
  nothing below 12 % of full scale is tinted.
- **Grouping follows the sort.** Sections (Apps / Background / Suspended) only when sorted by
  name; any other sort goes flat. A whole process tree goes into one section; Suspended wins
  over Apps.
- **Window titles are shown application-first**, but only when the trailing segment is
  corroborated against the exe's `FileDescription` or file name. Otherwise the title is shown
  verbatim. The name comes from the title so it is right on the first frame.
- **Escape is decided in one place at the end of the frame.** egui drops focus at the start of
  a pass containing Escape, so "was the caret in the filter?" must come from `lost_focus()`,
  not `has_focus()`.
- **The Name column is the stretching one**; dragging its right edge resizes the *next* column.
  The header sits outside the body's scroll area, so the scrollbar is always reserved.
- **New task opens `RunFileDlg`** on its own thread with no owner, placed by a `WH_CBT` hook at
  `HCBT_ACTIVATE`, centred under the button's centre and clamped to that monitor's work area.
- **Sensors: "—" for a reading the hardware lacks**, never 0. The *hotspot* card appears only
  where published. GPU backends are matched by PCI vendor id and position, never by name. ADL
  reports vendor id as decimal `1002`; both encodings are accepted and a test pins it.
- **Frame pacing needs `SeSystemProfilePrivilege`** — a platform limit, not a choice. It keys on
  `Microsoft-Windows-DXGI` id 42 and `Microsoft-Windows-DxgKrnl` id 184 (verified via TDH),
  counts both, and reports whichever saw more frames.
- **Sensors layout is keyed by what each item is**, not where it sits, so it survives driver
  updates and renumbered thermal zones.
- **Omissions that are platform limits:** no per-process network column (ETW + admin), no App
  history (SRUM), no startup impact (ETW boot traces), read-only services (SCM write needs
  elevation), no idle process row. Do not add fabricated figures for these.
- **Startup enable/disable writes Explorer's `StartupApproved` record** and deletes nothing.
  It takes effect at next sign-in; HKLM entries need elevation.

---

## Theme and design system

The window is `azur-egui-theme` throughout, down to the `TitleBar` (the window is created
with `with_decorations(false)`). Its egui must stay in lockstep with this crate's: two egui
versions are two `Context` types.

- **Vertical alignment is a standing rule.** Text on one line shares one baseline
  (`components::row_baseline`, `galley_baseline`); a row with a glyph uses `ink_baseline`; a
  control beside text is centred in the text's band; ask components for their height
  (`tabs_height`, `menu_item_height`, `menu_divider_height`). A `- 2.0` in a text position is
  the smell.
- `multisampling: 0` and `dithering: false` in [main.rs](src/main.rs) are deliberate: 1px rules
  and small text.

### The icon

`assets/app-source.png` (1254², no alpha) is the source; every other file in `assets/` is
reduced from it. Transparency is flood-filled from the border, not thresholded. Do not
premultiply by hand — Pillow already resamples RGBA premultiplied, and doing it twice put white
specks on the corner arcs.

| where | file | supplied by |
| --- | --- | --- |
| Explorer, shortcuts | `assets/app.ico`, nine PNG-compressed sizes | `build.rs` |
| running window's taskbar button | `assets/app-64.png` | `ViewportBuilder::with_icon` |
| title bar, 16-point box | `app-16/32/64.png`, nearest at or above the scale | `ui::app_mark` |

`build.rs` writes the `.res` itself (`RT_ICON`, `RT_GROUP_ICON`, `RT_VERSION`) — no `rc.exe`,
no build dependencies, no PNG decoder. `RT_VERSION` is what gives this exe a name in its own
Name/Publisher columns.

---

## Environment and conventions

- **Never `git add`** — leave work in the working tree.
- **Line endings are mixed, file by file** (about a third of `src/` is CRLF, e.g.
  `src/ui/table.rs`; others LF). A newline-joined search pattern can silently fail to match.
- Settings: `%APPDATA%\Azur Tasks\settings.ini`, written at most every two seconds and on exit.
  Columns keyed by stable id. Implausible values (tiny size, off-screen position) are ignored.
  Safe to delete.
- The window goes down to 500×400: the rail collapses to 48px icons, the filter shrinks to
  120px, then status-bar items drop a pair at a time rather than clipping.

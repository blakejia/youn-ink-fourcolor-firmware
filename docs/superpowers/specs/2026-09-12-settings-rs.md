# Settings menu → Rust (`settings.rs`)

## What was asked

The Settings page should become two-level: 系统 / 网络 / 关于 as level 1, with
系统 → 重启、重置网络, 网络 → current network status, 关于 unchanged. And: can the
page's C++ move to Rust?

Both questions have the same answer if the triage is done first. Applying
`port-candidacy-triage` to `ui/renderers/rawdraw/settings_renderer.cc` (1896 lines):

## Step 1 — liveness (evidence)

Checked every public entry point for callers outside the renderer:

| Surface | External callers | Verdict |
|---|---|---|
| `SetItems`, `UpdateItem`, `UpdateChecked`, `GetSelectedIndex`, `SetFirmwareVersion`, `SetDeviceInfo` | yes | live |
| About / Storage / Server / ServerList / Theme / OTA dialogs (`Show*`, `Hide*`, `Is*`, `Get*`, `Set*Handler`) | **0** | dead |
| `ShowVolumeDialog`, `SetVolumeDialogHandler` | **0** | dead (the two hits are `ChatRenderer`'s own same-named methods — the sibling-class form of the wrapper trap) |
| `GetItemCount`, `ShowDebugInfo`, `ShowCategoryHint`, `IsCategoryHintVisible`, `DrawChevron` | **0** | dead |

8 dialog render functions alone are 707 lines; with their input branches, header
state and the debug overlay, roughly 1000 of the 1896 lines are unreachable.

## Step 2 — classify

| Part | Owner | Move? |
|---|---|---|
| Frame/dialog drawing: fonts, framebuffer, theme, layout, the 关于 info panel | mechanism (panel + fonts) | no |
| Item values pushed from the device: SSID, IP, RSSI, reachability | mechanism (WiFi/HTTP reads) | no — stays, addressed by item id |
| Effects: restart, clear credentials, sleep, WiFi toggle | mechanism (IDF calls) | no |
| **Menu structure**: sections, items, labels, kinds, ids | policy (data + rules) | **yes** |
| **Navigation**: focus pane, cursor, which rows the cursor may rest on, what a gesture means | policy (pure) | **yes** |

The navigation is also exactly where the requested feature lives, so the port and
the feature are the same change.

## Structure (Rust, single source)

```
系统  → 重启 (action) · 重置网络 (action) · 省电模式 (action)
网络  → Wi-Fi (toggle) · 连接状态 (info) · IP 地址 (info) · 信号强度 (info) · 服务端 (info)
关于  → (no options; the renderer keeps drawing its info panel)
```

省电模式 is not in the requested list. It is a live action and has no other home,
so it stays in 系统; say the word and it moves or goes.

## Navigation rules

Cursor = `{ section, focus, option }`, focus ∈ {Nav, Options}.

- Nav: UP/DOWN move the section and clamp at the ends. BOOT enters the section's
  options — except 关于, which has none, so BOOT stays put there.
- Options: UP/DOWN move between selectable items and clamp; **UP at the first
  option returns to the nav** (that is the way back out, no new gesture, no
  routing-table change). BOOT activates an action or toggles a checkbox.
- The cursor never rests on an `info` row: those are read-outs, and a row that
  accepts a confirm and does nothing is the bug that started this (a chevron that
  lies, a confirm that is discarded).

## Boundary

`rf_settings_*` in `rust/include/settings.h`. The C++ gathers nothing and decides
nothing: it renders `(label, kind, value-by-id)` for the section Rust names,
highlights whatever Rust says is focused, and executes the effect Rust returns.
Labels cross as `&'static CStr`; values are filled per item id on the C++ side.

## Verification

- Host: structure + navigation tests, each shown to fail first, with sentinels
  (making info rows selectable must break the skip test; letting BOOT enter 关于
  must break the about test).
- Device: build, boot, and the panel drawn from the model; the button paths
  themselves need a human press (no injection), so the handover list goes in the
  report.
- Deletion of the ~1000 dead lines is **not** in this change: those dialogs are
  the plausible content for a future level 2 (音量/存储/主题/服务地址/OTA), and
  deleting vendor-written code is the user's call. Proposed, not done.

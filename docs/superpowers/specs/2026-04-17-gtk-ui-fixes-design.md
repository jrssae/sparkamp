# GTK UI Fixes — Design Spec
**Date:** 2026-04-17

Four targeted bug fixes for the Linux GTK4 frontend.

---

## 1. Playlist selection not using accent color

**File:** `frontends/gtk/style_dark.css`

**Root cause:**
- Line 349: `.playlist row:selected` uses `rgba(255, 255, 255, 0.25)` (hardcoded white) instead of `alpha(@accent_bg_color, 0.25)`.
- Line 444: A duplicate `.playlist row.drop-target` rule uses hardcoded `#003344`/`#00ccff`, overriding the correct accent-based rule at lines 352–355.

**Fix:**
- Change line 349 background to `alpha(@accent_bg_color, 0.25)`.
- Delete line 444 entirely (the correct rule at 352–355 already handles drop-target with accent color).

---

## 2. Settings tab: 3 active highlights → 1 underline

**File:** `frontends/gtk/style_dark.css`

**Root cause:** `notebook > header tab:checked` (lines 548–553) sets `color`, `border-top`, and `border-bottom` all in `@accent_bg_color`, creating three simultaneous visual highlights.

**Fix:**
Remove `color: @accent_bg_color` and `border-top: 2px solid @accent_bg_color`. Keep only:
```css
notebook > header tab:checked {
    background-color: #1a1a1a;
    border-bottom: 2px solid @accent_bg_color;
}
```

---

## 3. ML sidebar: no horizontal scrollbar + text showing from right

**File:** `frontends/gtk/window.rs`, sidebar_scroll construction (~line 8587)

**Root cause:**
- `hscrollbar_policy(PolicyType::Never)` suppresses the horizontal scrollbar entirely.
- GTK Label defaults to `xalign(0.5)` (centered text within the widget bounding box). Sidebar labels that `hexpand(true)` without `xalign(0.0)` display text centered, so when the sidebar is narrowed, the right portion of centered text is what falls in the visible area.
- `width_request(165)` on the ScrolledWindow is redundant now that a Paned controls width.

**Fix:**
- Change `hscrollbar_policy(PolicyType::Never)` → `PolicyType::Automatic`.
- Remove `width_request(165)` from the ScrolledWindow builder.
- Add `.xalign(0.0)` to every sidebar Label builder: Files row, Playlists header label, and all playlist sub-row labels.

---

## 4. ML column ordering controls in customize dialog

**Files:** `frontends/gtk/window.rs` — `open_customize_columns_dialog` and its ML window call site (~line 9562).

**Root cause:** `col_view.set_reorderable(true)` is set but column drag-reorder isn't functioning reliably. The customize dialog for `ColumnCustomizerMode::MediaLibrary` only has checkboxes, no ordering controls.

**Design:**

### Data model
Maintain an `Rc<RefCell<Vec<(String, bool)>>>` (id, visible) ordered list in the customizer. Initialize it from:
1. `ml_file_col_order` (for visible columns, in saved order)
2. Columns in `visible_columns` not yet in `ml_file_col_order` (appended)
3. Remaining hidden columns (appended at end)

### UI
Replace the simple checkbox list with a rebuild-based row list (same pattern as the ID3 field customizer):
- Each entry: `[▲] [▼] [☐] Label`
- ▲/▼ buttons only shown/enabled for visible columns (hidden columns have no meaningful position)
- Checkbox toggles visibility
- Instant save to config on every change (both order and visibility)

### Live preview
- On checkbox toggle: call existing `on_toggle(id, visible)` callback → `col.set_visible(visible)` in ColumnView
- On ▲/▼: save new `ml_file_col_order` to config immediately; do NOT reorder live ColumnView columns during the dialog (avoids complexity)

### On close
Add an `on_reorder: Option<Rc<dyn Fn()>>` parameter to `open_customize_columns_dialog`. At the call site in the ML window, pass a closure that reads `ml_file_col_order` from config and reorders the live ColumnView columns (remove + re-insert named columns in saved order, leaving the unscanned-indicator column at position 0).

### Signature change
```rust
fn open_customize_columns_dialog(
    parent: Option<&gtk4::Window>,
    state: Rc<RefCell<AppState>>,
    title: &str,
    mode: ColumnCustomizerMode,
    on_toggle: Option<Rc<dyn Fn(String, bool)>>,
    on_close: Option<Rc<dyn Fn()>>,   // existing param, renamed from None usage
    on_reorder: Option<Rc<dyn Fn()>>, // new: called on dialog close if order changed
)
```

The `on_reorder` closure in the ML window:
1. Reads `ml_file_col_order` from config
2. Iterates saved order; for each column ID at position `i`, if the ColumnView column is not already at position `i+1` (0 is unscanned indicator), calls `col_view.remove_column(col)` then `col_view.insert_column(i+1, col)`

---

## Testing
- `cargo build && cargo test` — zero warnings/failures
- Manual: change accent color in Settings, confirm playlist selection and seek bar match
- Manual: verify Settings tabs show single underline highlight
- Manual: narrow ML sidebar, confirm horizontal scrollbar appears and text starts at left
- Manual: open ML customize columns, reorder with ▲/▼, close — columns reorder in the file view

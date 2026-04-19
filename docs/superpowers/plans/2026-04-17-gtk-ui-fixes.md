# GTK UI Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix four Linux GTK4 UI regressions: playlist accent color mismatch, settings tab triple-highlight, ML sidebar scrollbar/alignment, and ML column ordering controls.

**Architecture:** All changes are confined to two files — `frontends/gtk/style_dark.css` (CSS fixes) and `frontends/gtk/window.rs` (Rust widget fixes). The ML column ordering feature replaces the simple checkbox list in `open_customize_columns_dialog` for `MediaLibrary` mode with a rebuild-based ordered list pattern, matching the existing ID3 field customizer design.

**Tech Stack:** Rust, GTK4 (gtk4-rs), CSS

---

## File Map

| File | Changes |
|------|---------|
| `Sparkamp/frontends/gtk/style_dark.css` | Issues 1 & 2: playlist selection, drop-target, settings tab |
| `Sparkamp/frontends/gtk/window.rs` | Issues 3 & 4: sidebar scroll, label alignment, column ordering UI |

---

### Task 1: Fix playlist selection accent color and drop-target rule

**Files:**
- Modify: `Sparkamp/frontends/gtk/style_dark.css`

Root cause: line 349 uses hardcoded white; line 444 has a duplicate `.playlist row.drop-target` rule with hardcoded cyan/teal that overrides the correct accent-based rule at line 352–355.

- [ ] **Step 1: Fix `.playlist row:selected` background**

In `style_dark.css`, find this block (around line 348):

```css
.playlist row:selected {
    background: rgba(255, 255, 255, 0.25);
}
```

Replace with:

```css
.playlist row:selected {
    background: alpha(@accent_bg_color, 0.25);
}
```

- [ ] **Step 2: Delete the hardcoded drop-target override**

Find and delete this entire line (around line 444). It is a single-line rule that overrides the correct accent-based rule defined earlier:

```css
.playlist row.drop-target { background-color: #003344; border-top: 2px solid #00ccff; }
```

After deletion, the drop-target rule at lines 352–355 takes effect:

```css
.playlist row.drop-target {
    border-top: 2px solid @accent_bg_color;
}
```

- [ ] **Step 3: Build and verify no errors**

```bash
cd /home/josef/Code/Sparkamp && cargo build 2>&1 | tail -5
```

Expected: `Finished` with zero errors.

- [ ] **Step 4: Commit**

```bash
cd /home/josef/Code/Sparkamp
git add frontends/gtk/style_dark.css
git commit -m "fix: playlist selection uses accent_bg_color, remove hardcoded drop-target override"
```

---

### Task 2: Fix settings tab to single underline highlight

**Files:**
- Modify: `Sparkamp/frontends/gtk/style_dark.css`

Root cause: `notebook > header tab:checked` sets text color, border-top, AND border-bottom all in the accent color, creating three simultaneous visual signals.

- [ ] **Step 1: Replace the tab:checked rule**

Find this block in `style_dark.css` (around line 548):

```css
notebook > header tab:checked {
    background-color: #1a1a1a;
    color: @accent_bg_color;
    border-top: 2px solid @accent_bg_color;
    border-bottom: 2px solid @accent_bg_color;
}
```

Replace with:

```css
notebook > header tab:checked {
    background-color: #1a1a1a;
    border-bottom: 2px solid @accent_bg_color;
}
```

- [ ] **Step 2: Build and verify**

```bash
cd /home/josef/Code/Sparkamp && cargo build 2>&1 | tail -5
```

Expected: `Finished` with zero errors.

- [ ] **Step 3: Commit**

```bash
cd /home/josef/Code/Sparkamp
git add frontends/gtk/style_dark.css
git commit -m "fix: settings tab active state shows single underline, not triple accent highlight"
```

---

### Task 3: Fix ML sidebar horizontal scrollbar and label alignment

**Files:**
- Modify: `Sparkamp/frontends/gtk/window.rs` (~lines 8587–8690)

Root cause: (a) `hscrollbar_policy(PolicyType::Never)` suppresses the horizontal scrollbar. (b) GTK Label defaults `xalign` to 0.5 (centered text within the widget). When labels have `hexpand(true)` but no `xalign(0.0)`, text is centered and the right half is visible when the sidebar is narrow. (c) `width_request(165)` on the ScrolledWindow is redundant now that a `Paned` controls the sidebar width.

- [ ] **Step 1: Fix the ScrolledWindow builder**

Find this block in `window.rs` (around line 8587):

```rust
    let sidebar_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .width_request(165)
        .vexpand(true)
        .child(&sidebar)
        .build();
```

Replace with:

```rust
    let sidebar_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Automatic)
        .vscrollbar_policy(PolicyType::Automatic)
        .vexpand(true)
        .child(&sidebar)
        .build();
```

- [ ] **Step 2: Add `xalign(0.0)` to the "Files" row label**

Find the "Files" label builder (around line 8597):

```rust
        let lbl = Label::builder()
            .label("Files")
            .halign(Align::Start)
            .margin_start(10)
            .margin_end(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
```

Replace with:

```rust
        let lbl = Label::builder()
            .label("Files")
            .halign(Align::Start)
            .xalign(0.0)
            .margin_start(10)
            .margin_end(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
```

- [ ] **Step 3: Add `xalign(0.0)` to the "Playlists" header label**

Find the `pl_lbl` builder (around line 8622):

```rust
        let pl_lbl = Label::builder()
            .label("Playlists")
            .halign(Align::Start)
            .hexpand(true)
            .margin_start(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
```

Replace with:

```rust
        let pl_lbl = Label::builder()
            .label("Playlists")
            .halign(Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .margin_start(10)
            .margin_top(7)
            .margin_bottom(7)
            .build();
```

- [ ] **Step 4: Add `xalign(0.0)` to playlist sub-row labels**

Find the label builder inside the `for pl in &playlists_initial` loop (around line 8677):

```rust
            let lbl = Label::builder()
                .label(&pl.name)
                .halign(Align::Start)
                .margin_start(24)  // indent
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();
```

Replace with:

```rust
            let lbl = Label::builder()
                .label(&pl.name)
                .halign(Align::Start)
                .xalign(0.0)
                .margin_start(24)  // indent
                .margin_end(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();
```

- [ ] **Step 5: Find and fix the dynamic playlist sub-row label builder**

There is a second label builder used when playlist rows are added dynamically (sidebar population after ML scans). Search for all other sidebar `Label::builder()` calls that include `.label(&pl.name)` or `.margin_start(24)` and add `.xalign(0.0)` to each one using the same pattern as Step 4.

Run:
```bash
grep -n "margin_start(24)" /home/josef/Code/Sparkamp/frontends/gtk/window.rs
```

For every match that is a sidebar playlist label, add `.xalign(0.0)` to the builder chain.

- [ ] **Step 6: Build and verify**

```bash
cd /home/josef/Code/Sparkamp && cargo build 2>&1 | tail -5
```

Expected: `Finished` with zero errors.

- [ ] **Step 7: Run tests**

```bash
cd /home/josef/Code/Sparkamp && cargo test 2>&1 | tail -10
```

Expected: all tests pass, 0 failures.

- [ ] **Step 8: Commit**

```bash
cd /home/josef/Code/Sparkamp
git add frontends/gtk/window.rs
git commit -m "fix: ML sidebar shows horizontal scrollbar and aligns text to start"
```

---

### Task 4: Add ordering controls to ML customize columns dialog

**Files:**
- Modify: `Sparkamp/frontends/gtk/window.rs` — `open_customize_columns_dialog` function (~lines 4889–5186)

The `ColumnCustomizerMode::Id3Editor` branch already exits early (calls `open_id3_field_customizer` and returns). Everything after that is the `MediaLibrary` branch. Replace the generic checkbox list with a rebuild-based ordered list that includes ▲/▼ buttons.

Also fix `open_customize_columns_dialog`'s close handlers to call `on_close` for ALL modes, not just `Id3Editor` — this lets us pass the column reorder callback as `on_close` from the ML window.

- [ ] **Step 1: Replace the MediaLibrary-mode body of `open_customize_columns_dialog`**

The function currently starts the MediaLibrary path after the early-return block for Id3Editor. Locate the section that begins at `ColumnCustomizerMode::Id3Editor => {` matching and ends before `let checkboxes:` (around line 5007). The block that sets up `cols_to_show`, `hdr_text`, the header label, `scrolled`, and `list_vbox` must be replaced.

Find the entire body of `open_customize_columns_dialog` after the early Id3Editor return (around line 4904) through the end of the function (around line 5185). **Replace everything from `let dlg = gtk4::Window::new();` through `dlg.present();`** with the implementation below.

The complete replacement for the body of `open_customize_columns_dialog` after the early Id3Editor return:

```rust
    let dlg = gtk4::Window::new();
    dlg.set_title(Some(title));
    dlg.set_default_size(400, 480);
    dlg.set_resizable(true);
    if let Some(p) = parent {
        dlg.set_transient_for(Some(p));
    }

    let main_vbox = GtkBox::new(Orientation::Vertical, 8);
    main_vbox.set_margin_top(12);
    main_vbox.set_margin_bottom(12);
    main_vbox.set_margin_start(12);
    main_vbox.set_margin_end(12);

    // ── Build ordered entry list ─────────────────────────────────────────────
    // Struct for each column entry
    #[derive(Clone)]
    struct ColEntry {
        id: String,
        header: String,
        visible: bool,
    }

    let saved_order = state.borrow().config.media_library.ml_file_col_order.clone();
    let visible_ids: Vec<String> = state.borrow().config.media_library.visible_columns.clone();
    let visible_set: std::collections::HashSet<String> = visible_ids.iter().cloned().collect();

    let mut init_entries: Vec<ColEntry> = Vec::new();
    // 1. Visible columns in saved order
    for id in &saved_order {
        if visible_set.contains(id) {
            if let Some(col) = ALL_COLUMNS.iter().find(|c| c.id == id.as_str()) {
                init_entries.push(ColEntry {
                    id: id.clone(),
                    header: col.header.to_string(),
                    visible: true,
                });
            }
        }
    }
    // 2. Visible columns not in saved order (e.g. newly added)
    for id in &visible_ids {
        if !saved_order.contains(id) {
            if let Some(col) = ALL_COLUMNS.iter().find(|c| c.id == id.as_str()) {
                init_entries.push(ColEntry {
                    id: id.clone(),
                    header: col.header.to_string(),
                    visible: true,
                });
            }
        }
    }
    // 3. Hidden columns (no order controls needed)
    for col in ALL_COLUMNS.iter() {
        if !visible_set.contains(col.id) {
            init_entries.push(ColEntry {
                id: col.id.to_string(),
                header: col.header.to_string(),
                visible: false,
            });
        }
    }

    let entries: Rc<RefCell<Vec<ColEntry>>> = Rc::new(RefCell::new(init_entries));

    // Persist entries → config on every change
    let save_cfg = {
        let entries = entries.clone();
        let st = state.clone();
        Rc::new(move || {
            let es = entries.borrow();
            let order: Vec<String> = es.iter().filter(|e| e.visible).map(|e| e.id.clone()).collect();
            let mut s = st.borrow_mut();
            s.config.media_library.visible_columns = order.clone();
            s.config.media_library.ml_file_col_order = order;
            let _ = s.config.save();
        })
    };

    // Header label
    let hdr = Label::builder()
        .label("Use ▲ ▼ to reorder visible columns:")
        .halign(Align::Start)
        .build();
    main_vbox.append(&hdr);

    let scrolled = ScrolledWindow::new();
    scrolled.set_hexpand(true);
    scrolled.set_vexpand(true);
    scrolled.set_has_frame(true);

    let list_lb = ListBox::new();
    list_lb.add_css_class("playlist");
    list_lb.set_selection_mode(gtk4::SelectionMode::None);

    scrolled.set_child(Some(&list_lb));
    main_vbox.append(&scrolled);

    // rebuild_holder so the rebuild closure can call itself
    let rebuild_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    let rebuild = {
        let entries = entries.clone();
        let lb_ref = list_lb.clone();
        let sc = save_cfg.clone();
        let rh = rebuild_holder.clone();
        let on_toggle_rb = on_toggle.clone();
        Rc::new(move || {
            while let Some(c) = lb_ref.first_child() {
                lb_ref.remove(&c);
            }

            let es = entries.borrow().clone();
            let visible_count = es.iter().filter(|e| e.visible).count();

            for (i, entry) in es.iter().enumerate() {
                let row_box = GtkBox::new(Orientation::Horizontal, 4);
                row_box.set_margin_top(2);
                row_box.set_margin_bottom(2);
                row_box.set_margin_start(4);
                row_box.set_margin_end(4);

                if entry.visible {
                    // ▲ button
                    let up_btn = Button::with_label("▲");
                    up_btn.add_css_class("pl-btn");
                    let visible_pos = es[..i].iter().filter(|e| e.visible).count();
                    up_btn.set_sensitive(visible_pos > 0);
                    if visible_pos > 0 {
                        let entries2 = entries.clone();
                        let sc2 = sc.clone();
                        let rh2 = rh.clone();
                        // find the previous visible entry's index
                        let prev_idx = es[..i].iter().rposition(|e| e.visible).unwrap();
                        up_btn.connect_clicked(move |_| {
                            entries2.borrow_mut().swap(i, prev_idx);
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }
                    row_box.append(&up_btn);

                    // ▼ button
                    let dn_btn = Button::with_label("▼");
                    dn_btn.add_css_class("pl-btn");
                    let visible_pos_after = es[i + 1..].iter().filter(|e| e.visible).count();
                    dn_btn.set_sensitive(visible_pos_after > 0);
                    if visible_pos_after > 0 {
                        let entries2 = entries.clone();
                        let sc2 = sc.clone();
                        let rh2 = rh.clone();
                        let next_idx = i + 1 + es[i + 1..].iter().position(|e| e.visible).unwrap();
                        dn_btn.connect_clicked(move |_| {
                            entries2.borrow_mut().swap(i, next_idx);
                            sc2();
                            if let Some(ref r) = *rh2.borrow() { r(); }
                        });
                    }
                    row_box.append(&dn_btn);
                } else {
                    // Spacer matching ▲▼ button widths so labels align
                    let spacer = GtkBox::new(Orientation::Horizontal, 4);
                    spacer.set_width_request(60); // approximate ▲+▼ width
                    row_box.append(&spacer);
                }

                // Visibility checkbox
                let cb = CheckButton::new();
                cb.set_active(entry.visible);
                {
                    let entries2 = entries.clone();
                    let sc2 = sc.clone();
                    let rh2 = rh.clone();
                    let on_tgl = on_toggle_rb.clone();
                    let idx = i;
                    cb.connect_toggled(move |btn| {
                        let visible = btn.is_active();
                        let id = entries2.borrow()[idx].id.clone();
                        entries2.borrow_mut()[idx].visible = visible;
                        sc2();
                        if let Some(ref cb) = on_tgl {
                            cb(id, visible);
                        }
                        if let Some(ref r) = *rh2.borrow() { r(); }
                    });
                }
                row_box.append(&cb);

                // Label
                let lbl = Label::builder()
                    .label(entry.header.as_str())
                    .halign(Align::Start)
                    .hexpand(true)
                    .xalign(0.0)
                    .build();
                if !entry.visible {
                    lbl.add_css_class("status-label");
                }
                row_box.append(&lbl);

                let row = ListBoxRow::new();
                row.set_child(Some(&row_box));
                lb_ref.append(&row);
            }
        })
    };

    *rebuild_holder.borrow_mut() = Some(rebuild.clone());
    rebuild();

    // ── Buttons row ──────────────────────────────────────────────────────────
    let btn_row = GtkBox::new(Orientation::Horizontal, 8);

    let btn_reset = Button::with_label("Reset Defaults");
    {
        let entries2 = entries.clone();
        let sc2 = save_cfg.clone();
        let rb2 = rebuild.clone();
        let on_tgl = on_toggle.clone();
        let st2 = state.clone();
        btn_reset.connect_clicked(move |_| {
            let defaults = crate::config::MediaLibraryConfig::default_visible_columns();
            let default_set: std::collections::HashSet<String> = defaults.iter().cloned().collect();
            {
                let mut es = entries2.borrow_mut();
                for e in es.iter_mut() {
                    e.visible = default_set.contains(&e.id);
                }
                // Re-sort: visible (in defaults order) then hidden
                es.sort_by_key(|e| {
                    if e.visible {
                        defaults.iter().position(|d| d == &e.id).unwrap_or(usize::MAX)
                    } else {
                        usize::MAX
                    }
                });
            }
            sc2();
            // Notify column view of visibility changes
            if let Some(ref cb) = on_tgl {
                for e in entries2.borrow().iter() {
                    cb(e.id.clone(), e.visible);
                }
            }
            // Update config visible_columns to defaults
            {
                let mut s = st2.borrow_mut();
                s.config.media_library.visible_columns = defaults.clone();
                s.config.media_library.ml_file_col_order = defaults;
                let _ = s.config.save();
            }
            rb2();
        });
    }
    btn_row.append(&btn_reset);

    let spring = GtkBox::new(Orientation::Horizontal, 0);
    spring.set_hexpand(true);
    btn_row.append(&spring);

    let btn_close = Button::with_label("Close");
    {
        let dlg_wk = dlg.downgrade();
        let oc = on_close.clone();
        btn_close.connect_clicked(move |_| {
            if let Some(ref cb) = oc { cb(); }
            if let Some(d) = dlg_wk.upgrade() { d.close(); }
        });
    }
    btn_row.append(&btn_close);

    main_vbox.append(&btn_row);
    dlg.set_child(Some(&main_vbox));

    dlg.connect_close_request(move |_| {
        if let Some(ref cb) = on_close { cb(); }
        glib::Propagation::Proceed
    });

    dlg.present();
```

- [ ] **Step 2: Build to catch type errors**

```bash
cd /home/josef/Code/Sparkamp && cargo build 2>&1 | grep -E "^error" | head -20
```

Fix any type errors before continuing. Common issues: `i` captured by move in closures (use `let idx = i;` before the closure), borrow conflicts (use `.borrow().clone()` to get owned data before closures).

- [ ] **Step 3: Run tests**

```bash
cd /home/josef/Code/Sparkamp && cargo test 2>&1 | tail -10
```

Expected: all tests pass, 0 failures.

- [ ] **Step 4: Commit**

```bash
cd /home/josef/Code/Sparkamp
git add frontends/gtk/window.rs
git commit -m "feat: ML customize columns dialog has up/down ordering controls"
```

---

### Task 5: Wire on_close reorder callback from ML window to customize dialog

**Files:**
- Modify: `Sparkamp/frontends/gtk/window.rs` — ML window's `btn_customize.connect_clicked` (~line 9562)

When the customize dialog closes, apply the saved `ml_file_col_order` to the live ColumnView.

- [ ] **Step 1: Replace the ML window's `btn_customize.connect_clicked` handler**

Find this block (around line 9560):

```rust
            let all_cols_rc = all_cols.clone();
            let win_wk = win.downgrade();
            btn_customize.connect_clicked(move |_| {
                let cols_for_callback = all_cols_rc.clone();
                open_customize_columns_dialog(
                    win_wk.upgrade().as_ref(),
                    state_rc.clone(),
                    "Customize Columns",
                    ColumnCustomizerMode::MediaLibrary,
                    Some(Rc::new(move |id: String, visible: bool| {
                        if let Some((_, col)) =
                            cols_for_callback.iter().find(|(col_id, _)| col_id == &id)
                        {
                            col.set_visible(visible);
                        }
                    }) as Rc<dyn Fn(String, bool)>),
                    None::<Rc<dyn Fn()>>,
                );
            });
```

Replace with:

```rust
            let all_cols_rc = all_cols.clone();
            let col_view_rc = col_view_holder.clone();
            let all_cols_holder_rc = all_cols_holder.clone();
            let state_for_reorder = state_rc.clone();
            let win_wk = win.downgrade();
            btn_customize.connect_clicked(move |_| {
                let cols_for_callback = all_cols_rc.clone();
                let cv_holder = col_view_rc.clone();
                let ac_holder = all_cols_holder_rc.clone();
                let st_reorder = state_for_reorder.clone();
                open_customize_columns_dialog(
                    win_wk.upgrade().as_ref(),
                    state_rc.clone(),
                    "Customize Columns",
                    ColumnCustomizerMode::MediaLibrary,
                    Some(Rc::new(move |id: String, visible: bool| {
                        if let Some((_, col)) =
                            cols_for_callback.iter().find(|(col_id, _)| col_id == &id)
                        {
                            col.set_visible(visible);
                        }
                    }) as Rc<dyn Fn(String, bool)>),
                    Some(Rc::new(move || {
                        let saved_order = st_reorder
                            .borrow()
                            .config
                            .media_library
                            .ml_file_col_order
                            .clone();
                        if saved_order.is_empty() { return; }
                        let cv_opt = cv_holder.borrow();
                        let ac_opt = ac_holder.borrow();
                        if let Some(col_view) = &*cv_opt {
                            let all_cols = &*ac_opt;
                            for (_, col) in all_cols.iter() {
                                col_view.remove_column(col);
                            }
                            let mut pos = 1u32;
                            for col_id in &saved_order {
                                if let Some((_, col)) =
                                    all_cols.iter().find(|(id, _)| id == col_id)
                                {
                                    col_view.insert_column(pos, col);
                                    pos += 1;
                                }
                            }
                            for (id, col) in all_cols.iter() {
                                if !saved_order.contains(id) {
                                    col_view.insert_column(pos, col);
                                    pos += 1;
                                }
                            }
                        }
                    }) as Rc<dyn Fn()>),
                );
            });
```

- [ ] **Step 2: Check that `col_view_holder` and `all_cols_holder` are in scope**

`col_view_holder` and `all_cols_holder` are declared around line 8704. The `btn_customize` click handler is inside the same `open_media_library_window` function scope, so they should be accessible. Verify by checking that `col_view_holder` and `all_cols_holder` are not moved into another closure before this point.

If `all_cols` (the local `Vec<(String, ColumnViewColumn)>` from inside the Files block) is already moved into `all_cols_holder` at line 9318, use `all_cols_holder` (the Rc holder) rather than `all_cols` (the local). The `all_cols_rc` in the on_toggle closure can be replaced with a read from `all_cols_holder` if needed, or kept separate since it's only used for visibility lookup.

- [ ] **Step 3: Build — resolve any lifetime or borrow errors**

```bash
cd /home/josef/Code/Sparkamp && cargo build 2>&1 | grep -E "^error" | head -30
```

Common fixes:
- Clone Rc values before the outer `move` closure: `let x = x.clone();` before `btn_customize.connect_clicked(move |_| {`
- If `state_rc` is moved into the on_toggle closure, clone it again before the on_close closure: `let st_reorder = state_rc.clone();` before the inner closure

- [ ] **Step 4: Run all tests**

```bash
cd /home/josef/Code/Sparkamp && cargo test 2>&1 | tail -10
```

Expected: all tests pass, 0 failures.

- [ ] **Step 5: Commit**

```bash
cd /home/josef/Code/Sparkamp
git add frontends/gtk/window.rs
git commit -m "feat: ML customize dialog triggers column reorder on close"
```

---

## Self-Review

**Spec coverage check:**
- Issue 1 (playlist accent color): Task 1 ✓
- Issue 2 (tab triple highlight): Task 2 ✓
- Issue 3 (sidebar scrollbar + alignment): Task 3 ✓ — all three label builders covered
- Issue 4 (column ordering controls): Tasks 4 & 5 ✓

**Placeholder scan:** No TBDs or vague instructions. Every code change is shown in full.

**Type consistency:**
- `ColEntry` struct defined and used only in Task 4's replacement block
- `open_customize_columns_dialog` signature unchanged (on_close was already `Option<Rc<dyn Fn()>>`)
- `col_view_holder`, `all_cols_holder` referenced consistently across Tasks 4 and 5
- `save_cfg` closure writes both `visible_columns` and `ml_file_col_order` in sync

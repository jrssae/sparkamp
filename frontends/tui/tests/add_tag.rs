//! Adding a tag the file does not already carry.
//!
//! The editor could only ever edit extra frames that were already in the file,
//! so a tag a user wanted to set for the first time was unreachable. `a` in the
//! Customize panel opens a picker of what this container can still hold.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

fn open_editor_on(path: &std::path::Path) -> App {
    let mut app = make_app();
    app.mode = Mode::Id3Editor(Id3EditorState {
        path: path.to_path_buf(),
        taggable: true,
        rows: crate::tui::id3_rows_for(path),
        tech_summary: String::new(),
        fields: Default::default(),
        rg_gain: String::new(),
        rg_seed: String::new(),
        focused: 0,
        cursor: 0,
        genre_sel: 0,
        show_extra: true,
        extra_frames: crate::id3_editor::read_extra_frames(path),
        extra_focused: 0,
        extra_editing: false,
        extra_input: String::new(),
        extra_cursor: 0,
        adding: false,
        add_choices: Vec::new(),
        add_focused: 0,
        status: None,
    });
    app
}

fn mp3(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("song.mp3");
    std::fs::write(&p, [0xFFu8, 0xFB, 0x90, 0x00]).unwrap();
    p
}

#[test]
fn a_opens_a_picker_of_frames_this_file_can_still_hold() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = open_editor_on(&mp3(dir.path()));

    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    let Mode::Id3Editor(ref s) = app.mode else { panic!("left the editor") };
    assert!(s.adding, "the picker is open");
    assert!(!s.add_choices.is_empty(), "and it has something to offer");
    assert!(
        s.add_choices.iter().any(|(id, l)| id == "TCMP" && l == "Compilation"),
        "including the tags this container can hold"
    );
    assert!(
        !s.add_choices.iter().any(|(id, _)| id == "TCOM"),
        "and not the main form's own fields"
    );
}

#[test]
fn enter_adds_the_chosen_frame_and_starts_editing_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = open_editor_on(&mp3(dir.path()));

    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    let chosen = match app.mode {
        Mode::Id3Editor(ref s) => s.add_choices[0].clone(),
        _ => panic!("left the editor"),
    };
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);

    let Mode::Id3Editor(ref s) = app.mode else { panic!("left the editor") };
    assert!(!s.adding, "the picker closed");
    assert_eq!(
        s.extra_frames.get(s.extra_focused).map(|f| f.id.as_str()),
        Some(chosen.0.as_str()),
        "the chosen frame is focused, ready for a value"
    );
    assert!(s.extra_editing, "and typing goes straight into it");
    assert_eq!(s.extra_input, "", "starting empty");
}

#[test]
fn esc_leaves_the_picker_without_adding_anything() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = open_editor_on(&mp3(dir.path()));
    let before = match app.mode {
        Mode::Id3Editor(ref s) => s.extra_frames.len(),
        _ => unreachable!(),
    };

    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);

    let Mode::Id3Editor(ref s) = app.mode else { panic!("left the editor") };
    assert!(!s.adding);
    assert!(s.show_extra, "and stays in the Customize panel");
    assert_eq!(s.extra_frames.len(), before, "nothing was added");
}

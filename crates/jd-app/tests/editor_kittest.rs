mod common;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use jd_app::app::JdUi;
use jd_core::command::{Dest, OpSource, VaultOp};
use jd_core::geom::Vec2;
use jd_core::id::NoteId;
use jd_core::session::SessionOp;
use jd_core::worker::{VaultCommand, VaultEvent};

/// Create an app with one card placed on the desk.
/// The note body is `body_text` (body-only, no frontmatter or heading in this helper).
/// Returns (vault_dir, harness, note_id).
fn app_with_one_card(body: &str) -> (common::TempDir, Harness<'static, JdUi>, NoteId) {
    let vault = common::temp_vault();
    let mut app = JdUi::new(vault.path()).expect("JdUi::new");

    // Create the note via worker (body only; common::new_note prepends "# Title\n").
    let seed = jd_core::note::NewNote {
        body: body.to_owned(),
        status: jd_core::note::Status::Permanent,
        kind: jd_core::note::Kind::Note,
        source: None,
        tags: Vec::new(),
    };
    app.vault
        .commands
        .send(VaultCommand::Op {
            op: VaultOp::Create {
                seed,
                dest: Dest::Notes,
            },
            source: OpSource::User,
        })
        .unwrap();

    let note_id;
    loop {
        match app
            .vault
            .events
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("OpDone")
        {
            VaultEvent::OpDone { result, .. } => {
                note_id = result.created.into_iter().next();
                break;
            }
            VaultEvent::ScanComplete { .. } => {
                app.state.scan_done = true;
                app.state.bodies.invalidate_all();
                if app.state.session.desks.is_empty() {
                    use jd_core::id::IdGen;
                    use jd_core::session::{DeskId, SessionOp, SurfaceId};
                    let mut id_gen = IdGen::new();
                    let desk_id = DeskId::generate(&mut id_gen);
                    let _ = app.state.session.apply(&SessionOp::CreateDesk {
                        id: desk_id,
                        name: "Desk".into(),
                    });
                    app.state.session.current_surface = Some(SurfaceId::Desk(desk_id));
                }
            }
            _ => continue,
        }
    }
    let note_id = note_id.expect("Create yielded an id");

    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui_state(|ui, app: &mut JdUi| app.ui(ui), app);

    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.scan_done && !a.state.session.desks.is_empty(),
        200,
        "scan + default desk",
    );

    let desk_id = h.state().state.session.desks[0].id;
    let _ = h.state_mut().state.session.apply(&SessionOp::Place {
        desk: desk_id,
        id: note_id,
        pos: Vec2 { x: 0.0, y: 0.0 },
    });
    h.run_ok();

    (vault, h, note_id)
}

/// Open the editor for the focused card. Focuses the card via ArrowRight then
/// presses Enter, and waits for the MultilineTextInput to appear with focus.
fn open_editor(h: &mut Harness<'_, JdUi>, id: NoteId) {
    // Focus the card via ArrowRight.
    h.key_press(egui::Key::ArrowRight);
    h.run_ok();
    assert_eq!(h.state().state.focus, Some(id), "focus must land on card");

    // Enter → DeskEvent::OpenCard → session.open_card; then body arrives → editor opens.
    h.key_press(egui::Key::Enter);
    common::pump(
        h,
        &mut |a: &JdUi| a.state.editor.is_some(),
        200,
        "editor opens",
    );
    // Run a few more frames to let the modal render and the TextEdit receive focus.
    for _ in 0..3 {
        h.step();
    }
    // Click the MultilineTextInput to ensure keyboard focus.
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .focus();
    h.get_by_role(egui::accesskit::Role::MultilineTextInput)
        .click();
    h.run_ok();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn enter_opens_editor_esc_saves_and_closes() {
    // Create a note whose body is "hello world".
    let (vault, mut h, id) = app_with_one_card("hello world");

    // Open the editor.
    open_editor(&mut h, id);

    // Editor widget must be present and focused (open_editor already called
    // node.focus() + node.click() + h.run_ok(); the desk fix ensures the card's
    // request_focus() does not steal focus back while the editor modal is open).
    h.get_by_role(egui::accesskit::Role::MultilineTextInput);

    // Drive typing through real egui events.
    // open_editor leaves the TextEdit focused; type_text queues Event::Text which
    // the focused TextEdit processes in the next step().
    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text(" again");
    h.step();
    h.run_ok();

    // Wait for Esc close + SaveBody OpDone.
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_none(),
        100,
        "editor closes on Esc",
    );

    // Wait for the SaveBody to complete (body cache invalidated → re-request will arrive).
    // Give the worker time to process SaveBody.
    std::thread::sleep(std::time::Duration::from_millis(200));
    h.run_ok();

    // Read the note file from disk to verify the body ends with "hello world again".
    let idx = h.state().vault.index.read().unwrap();
    let meta = idx.get(id).expect("note in index");
    let abs = vault.path().join(&meta.rel_path);
    drop(idx);
    let content = std::fs::read_to_string(&abs).expect("read note file");

    // Body must end with "hello world again" (body-only from file, after frontmatter).
    let doc = jd_core::doc::NoteDoc::parse(&content);
    assert!(
        doc.body.contains("hello world again"),
        "body must contain 'hello world again', got: {:?}",
        doc.body
    );

    // Round-trip check: frontmatter must start with the standard id line.
    let fm_text = content.split("---").nth(1).unwrap_or("");
    assert!(
        fm_text.contains("id:"),
        "frontmatter must survive untouched (must contain 'id:'), got: {:?}",
        &content[..content.len().min(200)]
    );

    // Editor is closed, open_card is cleared.
    assert_eq!(h.state().state.session.open_card, None);
    assert!(h.state().state.editor.is_none());
}

#[test]
fn autosave_fires_after_a_quiet_second() {
    let (vault, mut h, id) = app_with_one_card("initial body");

    open_editor(&mut h, id);
    h.get_by_role(egui::accesskit::Role::MultilineTextInput);

    // Directly mutate the editor buffer and mark dirty (simulates typing).
    if let Some(ed) = h.state_mut().state.editor.as_mut() {
        ed.buffer.push_str(" edited");
        ed.dirty = true;
        ed.last_edit = Some(std::time::Instant::now());
    }
    h.run_ok();

    // Wait > 1s without closing — autosave should fire.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    // Pump a few frames to let autosave fire.
    for _ in 0..20 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Editor must still be open.
    assert!(
        h.state().state.editor.is_some(),
        "editor must remain open after autosave"
    );

    // Give worker time to write.
    std::thread::sleep(std::time::Duration::from_millis(200));
    h.run_ok();

    // Disk shows the edit.
    let idx = h.state().vault.index.read().unwrap();
    let meta = idx.get(id).expect("note in index");
    let abs = vault.path().join(&meta.rel_path);
    drop(idx);
    let content = std::fs::read_to_string(&abs).expect("read note file");
    let doc = jd_core::doc::NoteDoc::parse(&content);
    assert!(
        doc.body.contains("edited"),
        "autosaved body must contain 'edited', got: {:?}",
        doc.body
    );
}

#[test]
fn open_close_without_edit_leaves_mtime_unchanged() {
    // Dirty-gate: opening and immediately closing the editor (no typing) must NOT
    // write the file, push a "Save body" journal entry, or invalidate the body cache
    // via the watcher echo.
    let (vault, mut h, id) = app_with_one_card("untouched body");

    // Capture the note file path and its mtime before opening.
    let abs = {
        let idx = h.state().vault.index.read().unwrap();
        let meta = idx.get(id).expect("note in index");
        vault.path().join(&meta.rel_path)
    };
    let mtime_before = std::fs::metadata(&abs)
        .expect("note file exists")
        .modified()
        .expect("mtime");

    // Record the journal depth before opening.
    let journal_len_before = h.state().state.journal.len();

    // Open the editor and immediately close with Esc — no typing.
    open_editor(&mut h, id);
    h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &jd_app::app::JdUi| a.state.editor.is_none(),
        100,
        "editor closes on Esc (no edit)",
    );

    // Give the worker a moment — if a SaveBody were sent it would arrive here.
    std::thread::sleep(std::time::Duration::from_millis(200));
    h.run_ok();

    // File mtime must be unchanged (no write happened).
    let mtime_after = std::fs::metadata(&abs)
        .expect("note file still exists")
        .modified()
        .expect("mtime after");
    assert_eq!(
        mtime_before, mtime_after,
        "clean open→close must not write the file (mtime changed)"
    );

    // No phantom journal entry must have been pushed for the SaveBody.
    let journal_len_after = h.state().state.journal.len();
    assert_eq!(
        journal_len_before, journal_len_after,
        "clean open→close must not push a phantom journal entry (was {journal_len_before}, now {journal_len_after})"
    );
}

#[test]
fn editor_styles_headings_larger() {
    use std::sync::{Arc, Mutex};

    // Create a note whose body begins with "# Big heading".
    let (_vault, mut h, _id) = app_with_one_card("# Big heading\nbody line");
    open_editor(&mut h, _id);

    // The editor must be open with a MultilineTextInput.
    h.get_by_role(egui::accesskit::Role::MultilineTextInput);

    // NOTE: This test covers layout_body in isolation (a standalone Harness below).
    // The wiring between JdUi, the modal TextEdit, and the layouter is exercised by
    // enter_opens_editor_esc_saves_and_closes and ctrl_enter_closes_too, which drive
    // real typing through the AccessKit node via the real editor modal.
    //
    // Galley row-height capture uses a standalone harness because egui's layouter
    // closure is local to editor_ui and cannot be extracted from the running JdUi
    // harness without invasive instrumentation.  The standalone harness runs the
    // same layout_body function with the same font installation path.
    let row_heights: Arc<Mutex<Option<(f32, f32)>>> = Arc::new(Mutex::new(None));
    let rh = row_heights.clone();

    // Use a standalone layouter harness (mirrors spike_layouter test).
    struct CaptureApp {
        rh: Arc<Mutex<Option<(f32, f32)>>>,
    }
    impl eframe::App for CaptureApp {
        fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
            let mut cache = jd_app::editor::LineCache::default();
            let galley = jd_app::editor::layout_body(
                ui,
                "# Big heading\nbody line",
                540.0,
                &mut cache,
                &|_| false,
                &jd_app::theme::Theme::light(),
            );
            if galley.rows.len() >= 2 {
                *self.rh.lock().unwrap() = Some((
                    galley.rows[0].rect().height(),
                    galley.rows[1].rect().height(),
                ));
            }
        }
    }

    let mut capture_h = Harness::builder().build_eframe(move |cc| {
        jd_app::theme::install_fonts(&cc.egui_ctx);
        CaptureApp { rh }
    });
    capture_h.run_ok();

    let guard = row_heights.lock().unwrap();
    let (heading_h, body_h) = guard.expect("galley rows captured");
    assert!(
        heading_h > body_h * 1.3,
        "heading row ({heading_h}) must be visibly taller than body row ({body_h})"
    );
}

#[test]
fn ctrl_enter_closes_too() {
    let (vault, mut h, id) = app_with_one_card("ctrl enter body");

    open_editor(&mut h, id);
    h.get_by_role(egui::accesskit::Role::MultilineTextInput);

    // Drive typing through real egui events (same pattern as
    // enter_opens_editor_esc_saves_and_closes).
    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text(" extra");
    h.step();
    h.run_ok();

    // Ctrl+Enter → CloseAndSave.
    h.event(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_none(),
        100,
        "editor closes on Ctrl+Enter",
    );

    // Give worker time to write.
    std::thread::sleep(std::time::Duration::from_millis(200));
    h.run_ok();

    // Verify saved.
    let idx = h.state().vault.index.read().unwrap();
    let meta = idx.get(id).expect("note in index");
    let abs = vault.path().join(&meta.rel_path);
    drop(idx);
    let content = std::fs::read_to_string(&abs).expect("read note file");
    let doc = jd_core::doc::NoteDoc::parse(&content);
    assert!(
        doc.body.contains("extra"),
        "ctrl+enter must save body, got: {:?}",
        doc.body
    );

    assert_eq!(h.state().state.session.open_card, None);
    assert!(h.state().state.editor.is_none());
}

#[test]
fn enter_continues_lists_and_empty_item_ends() {
    let (_vault, mut h, id) = app_with_one_card("");
    open_editor(&mut h, id);
    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text("- a");
    h.step();
    h.run_ok();
    // Enter on "- a" → next line auto-continues with "- ".
    h.key_press(egui::Key::Enter);
    h.step();
    h.run_ok();
    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert_eq!(buf, "- a\n- ", "Enter must continue the list with '- '");
    // Enter again on the empty item → prefix removed, list ended.
    h.key_press(egui::Key::Enter);
    h.step();
    h.run_ok();
    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert_eq!(
        buf, "- a\n",
        "Enter on the empty item must strip the prefix and end the list"
    );
    assert!(
        !buf.contains("- \n- "),
        "should not have double list prefix, got: {:?}",
        buf
    );
}

#[test]
fn link_autocomplete_inserts_a_resolved_link() {
    let (_vault, mut h, id) = app_with_one_card("");

    // Create a second note titled "Target Note" so the index can offer it.
    let seed = jd_core::note::NewNote {
        body: "# Target Note\ncontent".to_owned(),
        status: jd_core::note::Status::Permanent,
        kind: jd_core::note::Kind::Note,
        source: None,
        tags: Vec::new(),
    };
    h.state()
        .vault
        .commands
        .send(VaultCommand::Op {
            op: VaultOp::Create {
                seed,
                dest: Dest::Notes,
            },
            source: OpSource::User,
        })
        .unwrap();
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            a.vault
                .index
                .read()
                .unwrap()
                .resolve_title("Target Note")
                .is_some()
        },
        200,
        "Target Note indexed",
    );

    open_editor(&mut h, id);
    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text("[[Tar");
    h.step();
    h.step();
    h.run_ok();

    // The popup must show the candidate.
    h.get_by_label("Target Note");

    // Enter accepts the highlighted candidate.
    h.key_press(egui::Key::Enter);
    h.step();
    h.run_ok();
    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf.contains("[[Target Note]]"),
        "accepting the popup must insert the resolved link, got: {:?}",
        buf
    );
    assert!(
        !buf.contains("]]]]"),
        "closing brackets must not be doubled, got: {:?}",
        buf
    );
}

#[test]
fn typed_quotes_stay_ascii() {
    let (_vault, mut h, id) = app_with_one_card("");
    open_editor(&mut h, id);
    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text("\"hello\"");
    h.step();
    h.run_ok();
    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf.contains("\"hello\""),
        "typed quotes must stay ASCII, got: {:?}",
        buf
    );
    assert!(
        !buf.contains('\u{201C}'),
        "must not have U+201C, got: {:?}",
        buf
    );
    assert!(
        !buf.contains('\u{201D}'),
        "must not have U+201D, got: {:?}",
        buf
    );
}

#[test]
fn url_paste_over_selection_makes_md_link() {
    let (_vault, mut h, id) = app_with_one_card("docs");
    open_editor(&mut h, id);
    h.event(egui::Event::Key {
        key: egui::Key::A,
        physical_key: Option::None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    h.step();
    h.run_ok();
    h.event(egui::Event::Paste("https://example.com".to_owned()));
    h.step();
    h.run_ok();
    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf.contains("[docs](https://example.com)"),
        "url paste over selection must make markdown link, got: {:?}",
        buf
    );
}

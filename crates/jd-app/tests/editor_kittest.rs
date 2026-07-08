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
                false,
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

/// Undo stack survives editor close and reopen within the session.
/// Type "alpha beta", close (Esc), reopen the same card, Ctrl+Z → buffer "alpha ".
#[test]
fn text_undo_survives_close_and_reopen() {
    let (_vault, mut h, id) = app_with_one_card("");

    // Open editor and type "alpha beta" char by char so the word-boundary
    // grouping can fire (each char is a separate Event::Text frame).
    open_editor(&mut h, id);
    for ch in "alpha beta".chars() {
        h.event(egui::Event::Text(ch.to_string()));
        h.step();
        h.run_ok();
    }

    // Verify buffer matches.
    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf.contains("alpha beta"),
        "buffer must contain 'alpha beta' after typing, got: {:?}",
        buf
    );

    // Close with Esc — the editor is dirty so it sends SaveBody to the worker.
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_none(),
        100,
        "editor closes on Esc",
    );
    assert!(h.state().state.editor.is_none(), "editor must be closed");

    // Undo stack must be preserved in the text_undo map.
    assert!(
        h.state().state.text_undo.contains_key(&id),
        "text_undo map must preserve stack for the closed card"
    );

    // Wait for the worker to write the body to disk so reopen sees the new content.
    std::thread::sleep(std::time::Duration::from_millis(300));
    h.run_ok();

    // Open the card again: set open_card (as if user pressed Enter on card).
    // Bodies.get_or_request will fire a ReadBody if not cached; drain_events will
    // open the editor when the Body event arrives.
    h.state_mut().state.bodies.invalidate(id);
    h.state_mut().state.session.open_card = Some(id);

    // Pump until the editor opens again.
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_some(),
        200,
        "editor reopens",
    );
    // Settle focus.
    for _ in 0..3 {
        h.step();
    }

    let buf_after_reopen = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf_after_reopen.contains("alpha beta"),
        "editor must reopen with saved body 'alpha beta', got: {:?}",
        buf_after_reopen
    );

    // Press Ctrl+Z — the surviving undo stack should revert to "alpha "
    // (the first word group, before "beta" was typed).
    h.event(egui::Event::Key {
        key: egui::Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    h.step();
    h.run_ok();

    let buf_after_undo = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf_after_undo.contains("alpha ") && !buf_after_undo.contains("beta"),
        "Ctrl+Z after reopen must undo 'beta' → buffer 'alpha ', got: {:?}",
        buf_after_undo
    );
}

// ---------------------------------------------------------------------------
// Task 4: Promotion tests
// ---------------------------------------------------------------------------

/// Create an app with one FLEETING scrap placed on the desk.
/// Returns (vault_dir, harness, note_id).
fn app_with_fleeting_on_desk(body: &str) -> (common::TempDir, Harness<'static, JdUi>, NoteId) {
    let vault = common::temp_vault();
    let mut app = JdUi::new(vault.path()).expect("JdUi::new");

    let seed = jd_core::note::NewNote {
        body: body.to_owned(),
        status: jd_core::note::Status::Fleeting,
        kind: jd_core::note::Kind::Note,
        source: None,
        tags: Vec::new(),
    };
    app.vault
        .commands
        .send(VaultCommand::Op {
            op: VaultOp::Create {
                seed,
                dest: jd_core::command::Dest::Inbox,
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

/// The full pedagogy path (Task 4):
/// Create fleeting scrap, Enter at end → pending_promotion set + editor still open;
/// type body; Esc → file moves to notes/, body starts "# ", status Permanent,
/// journal has exactly ONE entry labeled with the scrap's first line.
#[test]
fn promotion_full_pedagogy_path() {
    let (vault, mut h, id) = app_with_fleeting_on_desk("egui layouter idea");

    // Verify the note starts fleeting.
    {
        let idx = h.state().vault.index.read().unwrap();
        let meta = idx.get(id).expect("note in index");
        assert_eq!(
            meta.status,
            jd_core::note::Status::Fleeting,
            "note must start fleeting"
        );
    }

    // Open the editor and verify is_fleeting is threaded.
    open_editor(&mut h, id);
    assert!(
        h.state().state.editor.as_ref().unwrap().is_fleeting,
        "editor must know the note is fleeting"
    );
    assert!(
        !h.state().state.editor.as_ref().unwrap().pending_promotion,
        "pending_promotion must be false before Enter"
    );

    // Capture journal length before promotion.
    let journal_len_before = h.state().state.journal.len();

    // Press Enter at end of the single-line buffer → triggers promotion.
    h.key_press(egui::Key::Enter);
    h.step();
    h.run_ok();

    // Editor must still be open.
    assert!(
        h.state().state.editor.is_some(),
        "editor must stay open after promotion Enter"
    );
    // pending_promotion must be set.
    assert!(
        h.state().state.editor.as_ref().unwrap().pending_promotion,
        "pending_promotion must be set after Enter at end of single-line"
    );
    // Buffer must have a newline (second line was added).
    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf.contains('\n'),
        "buffer must contain a newline after promotion Enter, got: {:?}",
        buf
    );

    // Type some body text.
    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text("body words");
    h.step();
    h.run_ok();

    // Close with Esc → compound Batch dispatched.
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_none(),
        100,
        "editor closes on Esc",
    );

    // Wait for the worker to process the Batch (SaveBody + Promote).
    common::pump(
        &mut h,
        &mut |a: &JdUi| {
            let idx = a.vault.index.read().unwrap();
            idx.get(id)
                .map(|m| m.status == jd_core::note::Status::Permanent)
                .unwrap_or(false)
        },
        200,
        "note promoted to Permanent",
    );

    // Give worker time to flush.
    std::thread::sleep(std::time::Duration::from_millis(200));
    h.run_ok();

    // File must be in notes/.
    {
        let idx = h.state().vault.index.read().unwrap();
        let meta = idx.get(id).expect("note must still be in index");
        assert_eq!(
            meta.status,
            jd_core::note::Status::Permanent,
            "promoted note must be Permanent"
        );
        // Path::starts_with compares components, so it is separator-agnostic
        // (Windows rel paths are "notes\\file.md").
        assert!(
            meta.rel_path.starts_with("notes"),
            "promoted note must be in notes/, got: {}",
            meta.rel_path.display()
        );
    }

    // Body must start with "# egui layouter idea".
    {
        let idx = h.state().vault.index.read().unwrap();
        let meta = idx.get(id).expect("note in index");
        let abs = vault.path().join(&meta.rel_path);
        drop(idx);
        let content = std::fs::read_to_string(&abs).expect("read promoted note");
        let doc = jd_core::doc::NoteDoc::parse(&content);
        assert!(
            doc.body.starts_with("# egui layouter idea"),
            "promoted body must start with '# egui layouter idea', got: {:?}",
            &doc.body[..doc.body.len().min(100)]
        );
    }

    // ONE journal entry labeled with the first line.
    let journal_len_after = h.state().state.journal.len();
    assert_eq!(
        journal_len_after,
        journal_len_before + 1,
        "promotion must journal exactly ONE entry (was {journal_len_before}, now {journal_len_after})"
    );
    assert_eq!(
        h.state().state.journal.undo_label(),
        Some("Promote scrap 'egui layouter idea'"),
        "journal label must name the first line"
    );

    // NOTE: Single Ctrl+Z full reversal (file back to inbox/, fleeting) lands
    // in Task 6's test once undo keys are wired to the app stack. Not testing here.
}

/// Multi-line scrap: Enter at end of line 2 does NOT trigger promotion.
#[test]
fn multiline_scrap_enter_does_not_promote() {
    let (_v, mut h, id) = app_with_fleeting_on_desk("line one");

    open_editor(&mut h, id);

    // Type a second line manually to make the buffer multi-line.
    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text("\nline two");
    h.step();
    h.run_ok();

    // Verify we now have a 2-line buffer.
    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf.contains('\n'),
        "buffer must be multi-line at this point"
    );

    // Press Enter — must NOT trigger promotion (not a single-line card anymore).
    h.key_press(egui::Key::Enter);
    h.step();
    h.run_ok();

    assert!(
        !h.state().state.editor.as_ref().unwrap().pending_promotion,
        "multi-line scrap Enter must NOT set pending_promotion"
    );

    // Status must still be fleeting.
    {
        let idx = h.state().vault.index.read().unwrap();
        let meta = idx.get(id).expect("note in index");
        assert_eq!(
            meta.status,
            jd_core::note::Status::Fleeting,
            "multi-line scrap must remain Fleeting after Enter"
        );
    }
}

/// Ctrl+Z while pending reverts the in-editor state (no vault op sent).
/// Sets pending_promotion, then Ctrl+Z → pending unset, no Batch dispatched.
#[test]
fn ctrl_z_while_pending_reverts_without_vault_op() {
    let (_v, mut h, _id) = app_with_fleeting_on_desk("my idea");

    open_editor(&mut h, _id);

    let journal_len_before = h.state().state.journal.len();

    // Enter at end → pending.
    h.key_press(egui::Key::Enter);
    h.step();
    h.run_ok();
    assert!(
        h.state().state.editor.as_ref().unwrap().pending_promotion,
        "pending_promotion must be set"
    );

    // Ctrl+Z → revert the newline + unset pending.
    h.event(egui::Event::Key {
        key: egui::Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    h.step();
    h.run_ok();

    assert!(
        !h.state().state.editor.as_ref().unwrap().pending_promotion,
        "Ctrl+Z must unset pending_promotion"
    );

    // Buffer must be back to single-line (no newline).
    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        !buf.contains('\n'),
        "Ctrl+Z must revert the promotion newline, buf: {:?}",
        buf
    );

    // Close without saving (clean close — no edit after undo).
    h.key_press(egui::Key::Escape);
    common::pump(
        &mut h,
        &mut |a: &JdUi| a.state.editor.is_none(),
        100,
        "editor closes",
    );

    // No new journal entry from the Ctrl+Z or close (no promotion, no save).
    // Give worker time to potentially send (should not).
    std::thread::sleep(std::time::Duration::from_millis(100));
    h.run_ok();

    let journal_len_after = h.state().state.journal.len();
    assert_eq!(
        journal_len_before, journal_len_after,
        "Ctrl+Z+close without edit must not push journal entry (was {journal_len_before}, now {journal_len_after})"
    );

    // Status must still be Fleeting.
    {
        let idx = h.state().vault.index.read().unwrap();
        let meta = idx.get(_id).expect("note in index");
        assert_eq!(
            meta.status,
            jd_core::note::Status::Fleeting,
            "Ctrl+Z revert must keep note Fleeting"
        );
    }
}

/// Finding 1: partial undo (body text after promoting Enter) must NOT clear pending_promotion.
/// Enter-promote → type body → one Ctrl+Z → pending STILL true, buffer has newline.
/// Second Ctrl+Z → pending false, single line.
#[test]
fn ctrl_z_partial_undo_preserves_pending_promotion() {
    let (_v, mut h, _id) = app_with_fleeting_on_desk("my scrap title");

    open_editor(&mut h, _id);

    // Enter at end → pending_promotion set, buffer = "my scrap title\n".
    h.key_press(egui::Key::Enter);
    h.step();
    h.run_ok();
    assert!(
        h.state().state.editor.as_ref().unwrap().pending_promotion,
        "pending_promotion must be set after Enter"
    );

    // Type some body text to create a new undo group after the newline.
    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text("body content");
    h.step();
    h.run_ok();

    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf.contains('\n'),
        "buffer must still have newline after body typing"
    );

    // First Ctrl+Z: reverts "body content" group. Buffer still has newline.
    h.event(egui::Event::Key {
        key: egui::Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    h.step();
    h.run_ok();

    let buf_after_first_undo = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf_after_first_undo.contains('\n'),
        "buffer must still contain newline after first Ctrl+Z (body group only reverted), got: {:?}",
        buf_after_first_undo
    );
    assert!(
        h.state().state.editor.as_ref().unwrap().pending_promotion,
        "pending_promotion must STILL be true after first Ctrl+Z (newline still present)"
    );

    // Second Ctrl+Z: reverts the newline. Buffer back to single line.
    h.event(egui::Event::Key {
        key: egui::Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    h.step();
    h.run_ok();

    let buf_after_second_undo = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        !buf_after_second_undo.contains('\n'),
        "buffer must be single-line after second Ctrl+Z, got: {:?}",
        buf_after_second_undo
    );
    assert!(
        !h.state().state.editor.as_ref().unwrap().pending_promotion,
        "pending_promotion must be false after second Ctrl+Z (newline gone)"
    );
}

/// Finding 4: empty fleeting buffer + Enter must NOT trigger promotion.
#[test]
fn empty_buffer_enter_does_not_promote() {
    let (_v, mut h, _id) = app_with_fleeting_on_desk("");

    open_editor(&mut h, _id);

    // Verify is_fleeting is set and buffer is empty.
    assert!(
        h.state().state.editor.as_ref().unwrap().is_fleeting,
        "editor must know the note is fleeting"
    );
    let buf = h.state().state.editor.as_ref().unwrap().buffer.clone();
    assert!(
        buf.trim().is_empty(),
        "buffer must be empty, got: {:?}",
        buf
    );

    // Press Enter on the empty buffer — must NOT trigger promotion.
    h.key_press(egui::Key::Enter);
    h.step();
    h.run_ok();

    assert!(
        !h.state().state.editor.as_ref().unwrap().pending_promotion,
        "Enter on empty buffer must NOT set pending_promotion"
    );
}

/// Finding 2: autosave must NOT fire while pending_promotion is true.
/// Directly set last_edit to a past time while pending and assert no SaveBody op arrives.
#[test]
fn autosave_suppressed_while_pending_promotion() {
    let (_v, mut h, _id) = app_with_fleeting_on_desk("autosave scrap");

    open_editor(&mut h, _id);

    // Enter → pending_promotion set.
    h.key_press(egui::Key::Enter);
    h.step();
    h.run_ok();
    assert!(
        h.state().state.editor.as_ref().unwrap().pending_promotion,
        "pending_promotion must be set"
    );

    // Directly set dirty + last_edit to a past time (> 1s ago) to simulate
    // autosave eligibility — same pattern as autosave_fires_after_a_quiet_second.
    if let Some(ed) = h.state_mut().state.editor.as_mut() {
        ed.dirty = true;
        // Force last_edit to 2 seconds ago so the autosave threshold is cleared.
        ed.last_edit = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    }

    // Record journal length before pumping — autosave must NOT add an entry.
    let journal_len_before = h.state().state.journal.len();

    // Pump several frames to give autosave a chance to fire (if it incorrectly would).
    for _ in 0..20 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Editor must still be open.
    assert!(h.state().state.editor.is_some(), "editor must remain open");

    // dirty must remain set (autosave was suppressed, did not clear it).
    assert!(
        h.state().state.editor.as_ref().unwrap().dirty,
        "dirty must remain true — autosave was suppressed by pending_promotion"
    );

    // pending_promotion must still be set (no spurious compound op sent).
    assert!(
        h.state().state.editor.as_ref().unwrap().pending_promotion,
        "pending_promotion must still be true"
    );

    // No extra journal entry (no autosave SaveBody was dispatched and completed).
    // Give worker a moment in case anything was sent.
    std::thread::sleep(std::time::Duration::from_millis(100));
    h.run_ok();
    let journal_len_after = h.state().state.journal.len();
    assert_eq!(
        journal_len_before, journal_len_after,
        "autosave while pending must not push a journal entry (was {journal_len_before}, now {journal_len_after})"
    );
}

/// Ctrl+Z in the editor must never push to the app journal.
#[test]
fn ctrl_z_in_editor_never_touches_the_app_journal() {
    let (_vault, mut h, id) = app_with_one_card("");

    open_editor(&mut h, id);
    let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    node.type_text("hello");
    h.step();
    h.run_ok();

    // Record journal length AFTER typing (typing itself may or may not add entries,
    // but Ctrl+Z must not add any new ones from that point).
    let journal_len_before = h.state().state.journal.len();

    // Press Ctrl+Z multiple times.
    for _ in 0..3 {
        h.event(egui::Event::Key {
            key: egui::Key::Z,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        h.step();
        h.run_ok();
    }

    let journal_len_after = h.state().state.journal.len();
    assert_eq!(
        journal_len_before, journal_len_after,
        "Ctrl+Z in editor must not push to the app journal (was {journal_len_before}, now {journal_len_after})"
    );
}

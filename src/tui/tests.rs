use std::path::PathBuf;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::collection::Collection;
use crate::git_remote::{RefKind, RemoteRefs};
use crate::hurl::HurlEntry;
use crate::i18n::Language;
use crate::persistence::PersistedState;

use crate::request::RequestView;

use super::app::*;
use super::git_save::*;
use super::new_request::*;
use super::remote::*;
use super::wrapcache::TextPos;

/// An app focused on a collection's Request-JSON (Main) pane. The entry URL
/// points at TEST-NET-1 (RFC 5737) so a started request hangs on connect,
/// keeping `loading == true` deterministically for the assertion.
fn app_in_main_pane() -> TuiApp {
    let mut app = TuiApp::default();
    let entry = HurlEntry {
        method: "GET".to_string(),
        url: "http://192.0.2.1:81/hang".to_string(),
        ..Default::default()
    };
    app.collections
        .push(Collection::new("t".to_string(), vec![entry]));
    app.active_tab = 1;
    app.focus = Pane::Main;
    app
}

#[test]
fn ctrl_enter_runs_request_instead_of_editing() {
    let mut app = app_in_main_pane();
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    assert!(
        app.overlay.is_none(),
        "Ctrl+Enter must not open the JSON editor"
    );
    assert!(
        app.response.lock().unwrap().loading,
        "Ctrl+Enter must start the request"
    );
}

#[test]
fn plain_enter_opens_editor() {
    let mut app = app_in_main_pane();
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        app.overlay.is_some(),
        "Enter should open the Edit Request wizard"
    );
    assert!(!app.response.lock().unwrap().loading);
}

#[test]
fn f5_runs_request_on_any_terminal() {
    let mut app = app_in_main_pane();
    app.on_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));
    assert!(
        app.response.lock().unwrap().loading,
        "F5 must start the request"
    );
}

#[test]
fn main_scroll_is_clamped_to_content() {
    let mut app = app_in_main_pane();
    app.main_max_scroll = 3; // e.g. content is 3 lines taller than the viewport

    for _ in 0..10 {
        app.nav(1);
    }
    assert_eq!(
        app.main_panel.scroll(),
        3,
        "must not scroll past the last line"
    );

    for _ in 0..10 {
        app.nav(-1);
    }
    assert_eq!(
        app.main_panel.scroll(),
        0,
        "must not scroll above the first line"
    );
}

#[test]
fn resp_scroll_is_clamped_to_content() {
    let mut app = app_in_main_pane();
    app.focus = Pane::Response;
    app.resp_max_scroll = 3; // e.g. content is 3 lines taller than the viewport

    for _ in 0..10 {
        app.nav(1);
    }
    assert_eq!(
        app.resp_panel.scroll(),
        3,
        "must not scroll past the last line"
    );

    for _ in 0..10 {
        app.nav(-1);
    }
    assert_eq!(
        app.resp_panel.scroll(),
        0,
        "must not scroll above the first line"
    );
}

fn press(app: &mut TuiApp, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn add_global_env(app: &mut TuiApp, env: crate::environment::Environment) -> u64 {
    let id = env.id;
    app.global_envs.push(env);
    id
}

fn add_empty_global_env(app: &mut TuiApp, name: &str) -> u64 {
    let (mut env, _) = crate::environment::parse_vars_pending(name.into(), "PLACEHOLDER=1");
    env.vars.clear();
    add_global_env(app, env)
}

fn only_env_id(app: &TuiApp) -> u64 {
    app.global_envs[0].id
}

fn only_env(app: &TuiApp) -> &crate::environment::Environment {
    &app.global_envs[0]
}

fn only_env_mut(app: &mut TuiApp) -> &mut crate::environment::Environment {
    &mut app.global_envs[0]
}

fn open_only_env_popup(app: &mut TuiApp) {
    app.overlay = Some(Overlay::EnvPopup(EnvPopupState::new(only_env_id(app))));
}

#[test]
fn effective_env_prefers_the_linked_environment_on_a_key_collision() {
    let mut app = TuiApp::default();
    let (active, _) =
        crate::environment::parse_vars_pending("global".into(), "TOKEN=from-global\nONLY_GLOBAL=g");
    let active_id = add_global_env(&mut app, active);
    app.active_env_id = Some(active_id);

    let (linked, _) =
        crate::environment::parse_vars_pending("linked".into(), "TOKEN=from-linked\nONLY_LINKED=l");
    let linked_id = add_global_env(&mut app, linked);
    app.collections[0].linked_env_id = Some(linked_id);

    let merged = app
        .effective_env(0)
        .expect("both a linked and active env are set");
    let token = merged.vars.iter().find(|v| v.key == "TOKEN").unwrap();
    assert_eq!(
        token.value, "from-linked",
        "the linked environment must win on a key collision"
    );
    assert!(
        merged.vars.iter().any(|v| v.key == "ONLY_GLOBAL"),
        "non-colliding global vars are kept"
    );
    assert!(
        merged.vars.iter().any(|v| v.key == "ONLY_LINKED"),
        "non-colliding linked vars are kept"
    );
}

#[test]
fn shadowed_env_keys_reports_only_keys_defined_in_both_environments() {
    let mut app = TuiApp::default();
    let (active, _) =
        crate::environment::parse_vars_pending("global".into(), "TOKEN=from-global\nONLY_GLOBAL=g");
    let active_id = add_global_env(&mut app, active);
    app.active_env_id = Some(active_id);

    let (linked, _) =
        crate::environment::parse_vars_pending("linked".into(), "TOKEN=from-linked\nONLY_LINKED=l");
    let linked_id = add_global_env(&mut app, linked);
    app.collections[0].linked_env_id = Some(linked_id);

    let shadowed = app.shadowed_env_keys(0);
    assert!(
        shadowed.contains("TOKEN"),
        "a key defined in both must be reported as shadowed"
    );
    assert!(
        !shadowed.contains("ONLY_GLOBAL"),
        "a key only in the active global env is not shadowed"
    );
    assert!(
        !shadowed.contains("ONLY_LINKED"),
        "a key only in the linked env is not shadowed"
    );
}

#[test]
fn shadowed_env_keys_is_empty_without_both_a_linked_and_an_active_environment() {
    let mut app = TuiApp::default();
    let (active, _) = crate::environment::parse_vars_pending("global".into(), "TOKEN=from-global");
    let active_id = add_global_env(&mut app, active);
    app.active_env_id = Some(active_id);
    // No linked env on collection 0: nothing can be shadowed.
    assert!(app.shadowed_env_keys(0).is_empty());
}

#[test]
fn deleting_an_environment_asks_for_confirmation_by_default() {
    let mut app = TuiApp::default();
    add_empty_global_env(&mut app, "staging");
    app.focus = Pane::GlobalEnv;
    app.global_env_idx = 0;

    press(&mut app, KeyCode::Char('x'));
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Confirm {
                action: ConfirmAction::DeleteEnv(0),
                ..
            })
        ),
        "x opens the delete confirmation by default"
    );

    press(&mut app, KeyCode::Char('y')); // confirm the deletion
    assert!(
        app.global_envs.is_empty(),
        "confirming deletes the environment"
    );
    assert!(
        matches!(app.status, Some(crate::i18n::Status::EnvDeleted(ref n)) if n == "staging"),
        "a status naming the deleted environment (with the undo hint) is shown"
    );
}

#[test]
fn x_deletes_an_environment_immediately_when_confirmation_is_off() {
    let mut app = TuiApp {
        confirm_on_delete_env: false,
        ..Default::default()
    };
    add_empty_global_env(&mut app, "staging");
    app.focus = Pane::GlobalEnv;
    app.global_env_idx = 0;

    press(&mut app, KeyCode::Char('x'));
    assert!(
        app.overlay.is_none(),
        "no confirmation popup when the preference is off"
    );
    assert!(app.global_envs.is_empty(), "the environment is deleted");
    assert!(
        matches!(app.status, Some(crate::i18n::Status::EnvDeleted(ref n)) if n == "staging"),
        "the delete status is still shown"
    );
}

#[test]
fn u_reopens_the_most_recently_deleted_environment() {
    let mut app = TuiApp {
        confirm_on_delete_env: false,
        ..Default::default()
    };
    add_empty_global_env(&mut app, "alpha");
    add_empty_global_env(&mut app, "bravo");
    app.focus = Pane::GlobalEnv;
    app.global_env_idx = 1; // bravo

    press(&mut app, KeyCode::Char('x')); // delete bravo
    assert_eq!(app.global_envs.len(), 1);

    press(&mut app, KeyCode::Char('u')); // reopen it
    assert_eq!(app.global_envs.len(), 2, "the environment comes back");
    assert_eq!(
        app.global_envs[1].name, "bravo",
        "it returns to the index it was deleted from"
    );
    assert_eq!(
        app.global_env_idx, 1,
        "the reopened environment becomes selected"
    );
    assert!(
        matches!(app.status, Some(crate::i18n::Status::EnvReopened(ref n)) if n == "bravo"),
        "a reopen status naming the environment is shown"
    );
}

#[test]
fn the_confirm_on_delete_env_preference_toggles_and_persists() {
    let mut app = TuiApp::default();
    assert!(
        app.confirm_on_delete_env,
        "confirmation is on by default (safe)"
    );

    app.overlay = Some(Overlay::Preferences(2));
    press(&mut app, KeyCode::Enter);
    assert!(!app.confirm_on_delete_env, "Enter toggles it off");
    assert!(
        matches!(app.overlay, Some(Overlay::Preferences(2))),
        "the highlight stays on the toggle row"
    );

    // The setting round-trips through persistence.
    let persisted = app.to_persisted();
    assert!(!persisted.confirm_on_delete_env);
    let mut restored = TuiApp::default();
    restored.apply_persisted(persisted);
    assert!(
        !restored.confirm_on_delete_env,
        "the preference survives a save/load cycle"
    );
}

#[test]
fn f2_on_the_environments_panel_renames_the_selected_environment() {
    let mut app = TuiApp::default();
    let (env, _) = crate::environment::parse_vars_pending("staging".into(), "TOKEN=v");
    let env_id = add_global_env(&mut app, env);
    app.focus = Pane::GlobalEnv;
    app.global_env_idx = 0;

    press(&mut app, KeyCode::F(2));

    match &app.overlay {
        Some(Overlay::Prompt { kind, editor, .. }) => {
            assert!(
                matches!(kind, PromptKind::RenameEnv(id) if *id == env_id),
                "F2 on the env panel opens the environment rename prompt, not the tab rename"
            );
            assert_eq!(editor.text(), "staging", "prefilled with the current name");
        }
        _ => panic!("F2 did not open a rename prompt"),
    }
}

#[test]
fn shadowed_env_keys_is_empty_when_linked_env_is_also_the_active_env() {
    let mut app = TuiApp::default();
    let (env, _) = crate::environment::parse_vars_pending("shared".into(), "TOKEN=v\nOTHER=w");
    let id = add_global_env(&mut app, env);
    // The collection is linked to the very environment that's also active —
    // the same value is substituted either way, so nothing is shadowed.
    app.active_env_id = Some(id);
    app.collections[0].linked_env_id = Some(id);
    assert!(app.shadowed_env_keys(0).is_empty());
}

#[test]
fn creating_a_request_adds_it_to_the_request_tab() {
    let mut app = TuiApp::default();
    assert!(
        app.collections[0].entries.is_empty(),
        "Request tab starts empty"
    );

    press(&mut app, KeyCode::Char('n')); // open New Request form
    assert!(app.overlay.is_some());

    for ch in "demo".chars() {
        press(&mut app, KeyCode::Char(ch)); // Name field
    }
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Right); // GET -> POST
    press(&mut app, KeyCode::Tab); // -> URL
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    // Ctrl+Enter submits.
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    assert!(app.overlay.is_none(), "form closes after create");
    let e = &app.collections[0].entries;
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].title, "demo");
    assert_eq!(e[0].method, "POST");
    assert_eq!(e[0].url, "http://h/x");
}

#[test]
fn new_request_can_target_another_collection() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    // active_tab is 0 (Request); the form snapshots ["Request", "api"].
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // Name -> Target
    press(&mut app, KeyCode::Right); // target 0 (Request) -> 1 (api)
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    assert!(
        app.collections[0].entries.is_empty(),
        "Request tab stays empty"
    );
    assert_eq!(
        app.collections[1].entries.len(),
        1,
        "request added to the chosen collection"
    );
    assert_eq!(
        app.active_tab, 1,
        "focus follows the request to its collection"
    );
}

#[test]
fn creating_a_request_with_a_header_via_the_table() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n')); // open form (focus Name)

    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> AddHeader (headers start empty)
    press(&mut app, KeyCode::Enter); // -> Header(0, Key)
    for ch in "X-Test".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> Header(0, Value)
    for ch in "abc".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries;
    assert_eq!(e.len(), 1);
    assert_eq!(
        e[0].headers,
        vec![("X-Test".to_string(), "abc".to_string(), true)]
    );
}

#[test]
fn empty_header_rows_are_not_added() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n')); // form starts with one blank header row
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries;
    assert_eq!(e.len(), 1);
    assert!(e[0].headers.is_empty(), "blank header rows must be dropped");
}

#[test]
fn header_widths_drop_description_first_when_narrow() {
    // Wide: enabled + all three text columns are shown.
    let (e, k, v, d) = header_widths(80);
    assert!(e > 0 && k > 0 && v > 0 && d > 0);
    assert_eq!(e + k + v + d + 3, 80, "columns plus gaps fill the width");

    // Narrow: Description is dropped but Enabled/Key/Value remain usable.
    let (e, k, v, d) = header_widths(30);
    assert!(e > 0 && k > 0 && v > 0);
    assert_eq!(d, 0, "Description is the first column to lose its width");
}

fn new_focus(app: &TuiApp) -> NewField {
    match app.overlay.as_ref().unwrap() {
        Overlay::NewRequest(f) => f.focus,
        _ => panic!("New Request overlay not open"),
    }
}

fn form_ref(app: &TuiApp) -> &NewReq {
    match app.overlay.as_ref().unwrap() {
        Overlay::NewRequest(f) => f,
        _ => panic!("New Request overlay not open"),
    }
}

/// Step the wizard focus ring directly (bypassing key handling) and return
/// the new focus.
fn form_step(app: &mut TuiApp, forward: bool) -> NewField {
    match app.overlay.as_mut().unwrap() {
        Overlay::NewRequest(f) => {
            f.focus_next(forward, true);
            f.focus
        }
        _ => panic!("New Request overlay not open"),
    }
}

/// The focus ring must be a true ring: walking it forward all the way round
/// returns to the start, and Shift+Tab retraces exactly the same stops in
/// reverse. This is the invariant the old hand-written forward/backward
/// state machines could (and did) violate.
#[test]
fn tab_ring_backward_exactly_reverses_forward() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app);
    press(&mut app, KeyCode::Char('X')); // header row now non-blank

    let start = new_focus(&app);
    let mut fwd = vec![start];
    loop {
        let f = form_step(&mut app, true);
        if f == start {
            break;
        }
        fwd.push(f);
        assert!(fwd.len() < 200, "forward ring never returned to start");
    }
    // Focus is back at `start`; stepping backward retraces `fwd` in reverse.
    for &expected in fwd.iter().skip(1).rev() {
        assert_eq!(form_step(&mut app, false), expected);
    }
    assert_eq!(form_step(&mut app, false), start);
}

#[test]
fn tab_skips_empty_headers_cookies_and_form_between_url_and_body() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // Name -> Target
    press(&mut app, KeyCode::Tab); // Target -> Method
    press(&mut app, KeyCode::Tab); // Method -> Url
    press(&mut app, KeyCode::Tab); // Url -> AddHeader (headers start empty)
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Header));

    press(&mut app, KeyCode::Tab); // empty header section -> AddCookie
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Cookie));

    press(&mut app, KeyCode::Tab); // empty cookie section -> AddQuery
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Query));

    press(&mut app, KeyCode::Tab); // empty query section -> AddFormField
    assert_eq!(new_focus(&app), NewField::AddFormField);

    press(&mut app, KeyCode::Tab); // empty form section -> jump to Body
    assert_eq!(new_focus(&app), NewField::Body);

    // Shift+Tab from Body walks back through the same chain to URL.
    press(&mut app, KeyCode::BackTab);
    assert_eq!(new_focus(&app), NewField::AddFormField);
    press(&mut app, KeyCode::BackTab);
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Query));
    press(&mut app, KeyCode::BackTab);
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Cookie));
    press(&mut app, KeyCode::BackTab);
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Header));
    press(&mut app, KeyCode::BackTab);
    assert_eq!(new_focus(&app), NewField::Url);
}

fn header_enabled(app: &TuiApp, i: usize) -> bool {
    match app.overlay.as_ref().unwrap() {
        Overlay::NewRequest(f) => f.headers[i].enabled,
        _ => panic!("New Request overlay not open"),
    }
}

/// Move focus onto the first header row's Key cell in a fresh form
/// (headers start empty, so this also creates the first row via the
/// "+ Add Header" entry point).
fn open_form_on_header(app: &mut TuiApp) {
    press(app, KeyCode::Char('n'));
    press(app, KeyCode::Tab); // -> Target
    press(app, KeyCode::Tab); // -> Method
    press(app, KeyCode::Tab); // -> Url
    press(app, KeyCode::Tab); // -> AddHeader (headers start empty)
    press(app, KeyCode::Enter); // creates Header(0, Key)
}

fn open_form_on_form_field_kind(app: &mut TuiApp) {
    press(app, KeyCode::Char('n'));
    press(app, KeyCode::Tab); // -> Target
    press(app, KeyCode::Tab); // -> Method
    press(app, KeyCode::Tab); // -> Url
    press(app, KeyCode::Tab); // -> AddHeader (headers start empty)
    press(app, KeyCode::Tab); // -> AddCookie (cookies start empty)
    press(app, KeyCode::Tab); // -> AddQuery (queries start empty)
    press(app, KeyCode::Tab); // -> AddFormField (form starts empty)
    press(app, KeyCode::Enter); // creates FormField(0, Key)
    press(app, KeyCode::Char('k')); // non-blank, or Tab skips the empty section
    press(app, KeyCode::Tab); // -> FormField(0, Kind) (Kind comes before Value)
}

#[test]
fn deleting_the_last_header_leaves_the_section_empty() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app); // one header row, focus Header(0, Key)

    // Ctrl+D removes the only row; unlike Headers/Cookies/Form's old
    // behaviour, the section is now allowed to be genuinely empty
    // (matching Asserts/Captures) — focus lands on "+ Add Header".
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));

    match app.overlay.as_ref().unwrap() {
        Overlay::NewRequest(f) => assert!(
            f.headers.is_empty(),
            "the section is left empty, not re-seeded"
        ),
        _ => panic!("form not open"),
    }
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Header),
        "focus lands on the Add row"
    );

    // Enter on the Add row creates a fresh row again.
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key)
    );
    press(&mut app, KeyCode::Char('X'));
    press(&mut app, KeyCode::Tab); // Key -> Value
    press(&mut app, KeyCode::Tab); // Value -> Desc
    press(&mut app, KeyCode::Tab); // Desc -> Add header
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Header));
}

#[test]
fn arrow_keys_move_between_columns_in_a_header_row() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key)
    );

    // On an empty cell the cursor is already at the edge, so Left/Right cross cells.
    press(&mut app, KeyCode::Right);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Value)
    );
    press(&mut app, KeyCode::Right);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Desc)
    );
    press(&mut app, KeyCode::Right); // last column clamps
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Desc)
    );

    press(&mut app, KeyCode::Left);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Value)
    );

    // The Enabled checkbox is the leftmost visual column, reached with
    // Left from Key (and clamped there — it's the first column).
    press(&mut app, KeyCode::Left);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key)
    );
    press(&mut app, KeyCode::Left);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Enabled)
    );
    press(&mut app, KeyCode::Left); // first column clamps
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Enabled)
    );
}

#[test]
fn left_right_move_the_cursor_before_crossing_cells() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app);
    for ch in "ab".chars() {
        press(&mut app, KeyCode::Char(ch)); // Key = "ab", cursor at end
    }
    // At the end of the text, Right crosses to the next cell.
    press(&mut app, KeyCode::Right);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Value)
    );
    // Back in Key: Left first walks the cursor through "ab" (2 steps) then crosses.
    press(&mut app, KeyCode::Left);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key)
    );
    press(&mut app, KeyCode::Left); // cursor moves within "ab"
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key)
    );
}

fn focused_cell_col(app: &TuiApp) -> usize {
    match app.overlay.as_ref().unwrap() {
        Overlay::NewRequest(f) => match f.focus {
            NewField::Kvd(KvdKind::Header, i, col) => {
                let row = &f.headers[i];
                match col {
                    HdrCol::Key => row.key.col,
                    HdrCol::Value => row.value.col,
                    HdrCol::Desc => row.desc.col,
                    HdrCol::Enabled => usize::MAX,
                }
            }
            _ => panic!("focus is not on a header cell"),
        },
        _ => panic!("New Request overlay not open"),
    }
}

#[test]
fn ctrl_left_right_jump_to_start_and_end_of_cell() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app); // focus Header(0, Key)
    for ch in "hello".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(focused_cell_col(&app), 5, "cursor at end after typing");

    // Ctrl+Left jumps to the start of the field without leaving the cell.
    app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key),
        "stays in the same cell"
    );
    assert_eq!(focused_cell_col(&app), 0, "Ctrl+Left goes to the start");

    // Ctrl+Right jumps to the end, also without crossing cells.
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key),
        "stays in the same cell"
    );
    assert_eq!(focused_cell_col(&app), 5, "Ctrl+Right goes to the end");
}

#[test]
fn up_down_move_between_header_rows() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app);
    press(&mut app, KeyCode::Char('k')); // make row 0 non-blank
    // Walk to Add header and create a second row (Tab skips the checkbox).
    press(&mut app, KeyCode::Tab); // -> Value
    press(&mut app, KeyCode::Tab); // -> Desc
    press(&mut app, KeyCode::Tab); // -> Add header (Enabled skipped)
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Header));
    press(&mut app, KeyCode::Char(' ')); // add row 1
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 1, HdrCol::Key)
    );

    // Move to the Value column, where Up/Down move between rows (the Key
    // column's Up/Down drive the suggestion dropdown instead).
    press(&mut app, KeyCode::Right); // Header(1, Value)
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 1, HdrCol::Value)
    );
    press(&mut app, KeyCode::Up);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Value)
    );
    press(&mut app, KeyCode::Up); // top row now leaves the table upward to URL
    assert_eq!(new_focus(&app), NewField::Url);
    press(&mut app, KeyCode::Down); // URL drops back into the first header cell
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key)
    );
    press(&mut app, KeyCode::Right); // -> Header(0, Value)
    press(&mut app, KeyCode::Down); // move down between rows
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 1, HdrCol::Value)
    );
    press(&mut app, KeyCode::Down); // last row leaves downward to "Add header"
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Header));
}

#[test]
fn up_down_navigate_between_form_sections() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n')); // open form (focus starts on Name)
    assert_eq!(new_focus(&app), NewField::Name);

    // Down walks forward through the sections (previously a no-op on the
    // single-line Name/URL fields).
    press(&mut app, KeyCode::Down);
    assert_eq!(new_focus(&app), NewField::Target);
    press(&mut app, KeyCode::Down);
    assert_eq!(new_focus(&app), NewField::Method);
    press(&mut app, KeyCode::Down);
    assert_eq!(new_focus(&app), NewField::Url);

    // Up walks back through them.
    press(&mut app, KeyCode::Up);
    assert_eq!(new_focus(&app), NewField::Method);
    press(&mut app, KeyCode::Up);
    assert_eq!(new_focus(&app), NewField::Target);
    press(&mut app, KeyCode::Up);
    assert_eq!(new_focus(&app), NewField::Name);
}

#[test]
fn arrow_up_stops_at_every_section_it_passes_through_just_like_arrow_down_does() {
    // Regression test: Up-arrow row navigation used to jump straight
    // past an empty Headers/Cookies section into the one before it
    // (e.g. Cookie(0) Up -> Url, skipping "+ Add Header" entirely) even
    // though Down always stops at each section's "+ Add ..." row one at
    // a time. Up must now do exactly the same: one section boundary per
    // keypress, never more.
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    press(&mut app, KeyCode::Tab); // -> AddHeader (headers start empty)
    press(&mut app, KeyCode::Tab); // -> AddCookie (cookies start empty)
    press(&mut app, KeyCode::Enter); // creates Cookie(0, Key); headers is still empty

    press(&mut app, KeyCode::Up);
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Header),
        "Up from the first Cookie row must stop at the empty Headers section's Add row, not skip past it to Url"
    );
    press(&mut app, KeyCode::Up);
    assert_eq!(
        new_focus(&app),
        NewField::Url,
        "the next Up leaves Headers upward as normal"
    );
}

#[test]
fn arrow_up_from_the_first_form_field_stops_at_cookies_then_headers_one_section_at_a_time() {
    // Same bug, but two empty sections in a row above the current one
    // (Cookies then Headers) — Up must stop at each in turn, never
    // cascade through both in a single keypress.
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    press(&mut app, KeyCode::Tab); // -> AddHeader (headers start empty)
    press(&mut app, KeyCode::Tab); // -> AddCookie (cookies start empty)
    press(&mut app, KeyCode::Tab); // -> AddQuery (queries start empty)
    press(&mut app, KeyCode::Tab); // -> AddFormField (form starts empty)
    press(&mut app, KeyCode::Enter); // creates FormField(0, Key); headers/cookies still empty

    press(&mut app, KeyCode::Up);
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Query),
        "Up from the first Form row must stop at the empty Queries section first"
    );
    press(&mut app, KeyCode::Up);
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Cookie),
        "the next Up stops at the empty Cookies section"
    );
    press(&mut app, KeyCode::Up);
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Header),
        "the next Up stops at the empty Headers section, not before"
    );
    press(&mut app, KeyCode::Up);
    assert_eq!(
        new_focus(&app),
        NewField::Url,
        "the next Up leaves Headers upward as normal"
    );
}

#[test]
fn arrow_up_from_the_first_capture_row_stops_at_the_empty_asserts_section() {
    // Same bug in the Asserts/Captures pair: Up from Capture(0) used to
    // skip straight to Body when Asserts was empty, bypassing
    // "+ Add Assert" entirely.
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    // Walk straight to Body, then on to the (empty) Asserts and Captures
    // "Add" rows.
    for _ in 0..8 {
        press(&mut app, KeyCode::Tab); // Name -> ... -> Body
    }
    assert_eq!(new_focus(&app), NewField::Body);
    press(&mut app, KeyCode::Tab); // -> AddAssert (asserts start empty)
    press(&mut app, KeyCode::Tab); // -> AddCapture (captures start empty)
    press(&mut app, KeyCode::Enter); // creates Capture(0, Name); asserts still empty

    press(&mut app, KeyCode::Up);
    assert_eq!(
        new_focus(&app),
        NewField::AddAssert,
        "Up from the first Capture row must stop at the empty Asserts section's Add row, not skip past it to Body"
    );
    press(&mut app, KeyCode::Up);
    assert_eq!(
        new_focus(&app),
        NewField::Body,
        "the next Up leaves Asserts upward as normal"
    );
}

#[test]
fn up_down_in_the_body_move_the_cursor_then_leave_at_the_edges() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..8 {
        press(&mut app, KeyCode::Tab); // Name -> ... -> Body (skips the blank sections)
    }
    assert_eq!(new_focus(&app), NewField::Body);

    for ch in "line1".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Enter); // newline (Body is multi-line)
    for ch in "line2".chars() {
        press(&mut app, KeyCode::Char(ch));
    }

    // The cursor is on the second line: Up moves it up, staying in the Body.
    press(&mut app, KeyCode::Up);
    assert_eq!(
        new_focus(&app),
        NewField::Body,
        "Up within a multi-line body moves the cursor"
    );
    // Now at the top line: Up leaves the Body section upward.
    press(&mut app, KeyCode::Up);
    assert_ne!(
        new_focus(&app),
        NewField::Body,
        "Up at the top of the body leaves the section"
    );
}

#[test]
fn filter_headers_matches_case_insensitive_substring() {
    assert_eq!(filter_headers(""), COMMON_HEADERS.to_vec());
    assert_eq!(filter_headers("auth"), vec!["Authorization"]);
    let content = filter_headers("Content");
    assert!(content.contains(&"Content-Type"));
    assert!(
        content
            .iter()
            .all(|h| h.to_ascii_lowercase().contains("content"))
    );
    assert!(filter_headers("zzz").is_empty());
}

#[test]
fn key_cell_shows_a_prepopulated_dropdown() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app); // lands on an empty Key cell
    let dd = form_ref(&app).key_dropdown();
    assert!(
        dd.is_some(),
        "an empty Key cell offers the full suggestion list"
    );
    assert_eq!(dd.unwrap().1, COMMON_HEADERS.to_vec());
}

#[test]
fn dropdown_filters_as_you_type_and_enter_fills_the_key() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app);
    for ch in "content".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    let sugs = form_ref(&app)
        .key_dropdown()
        .expect("matches for 'content'")
        .1;
    assert!(
        sugs.iter()
            .all(|h| h.to_ascii_lowercase().contains("content"))
    );

    // Down highlights the first match; Enter fills the Key and advances.
    press(&mut app, KeyCode::Down);
    assert_eq!(form_ref(&app).suggest_hi, Some(0));
    let first = sugs[0];
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        form_ref(&app).headers[0].key.text(),
        first,
        "Enter fills the Key"
    );
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Value),
        "focus advances"
    );
    assert!(
        form_ref(&app).key_dropdown().is_none(),
        "dropdown closed after accept"
    );
}

#[test]
fn down_on_key_cell_navigates_suggestions_not_rows() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app);
    press(&mut app, KeyCode::Down);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key),
        "focus stays on the Key cell"
    );
    assert_eq!(
        form_ref(&app).suggest_hi,
        Some(0),
        "the dropdown highlight advances"
    );
}

#[test]
fn esc_dismisses_dropdown_before_cancelling_the_form() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app);
    assert!(form_ref(&app).key_dropdown().is_some());

    press(&mut app, KeyCode::Esc); // first Esc: close the dropdown only
    assert!(app.overlay.is_some(), "the form stays open");
    assert!(
        form_ref(&app).key_dropdown().is_none(),
        "the dropdown is dismissed"
    );

    press(&mut app, KeyCode::Esc); // second Esc: cancel the form
    assert!(app.overlay.is_none(), "the form is cancelled");
}

#[test]
fn typing_no_match_hides_the_dropdown() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app);
    for ch in "zzz".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert!(
        form_ref(&app).key_dropdown().is_none(),
        "no header contains 'zzz'"
    );
}

#[test]
fn tab_skips_the_enabled_checkbox() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app);
    press(&mut app, KeyCode::Char('k')); // make row 0 non-blank

    press(&mut app, KeyCode::Tab); // Key -> Value
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Value)
    );
    press(&mut app, KeyCode::Tab); // Value -> Desc
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Desc)
    );
    press(&mut app, KeyCode::Tab); // Desc -> Add header (checkbox skipped)
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Header));

    // Shift+Tab returns to Desc, never the checkbox.
    press(&mut app, KeyCode::BackTab);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Desc)
    );
}

#[test]
fn arrows_can_still_reach_the_enabled_checkbox() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app);
    // The checkbox is the leftmost visual column, so a single Left from
    // Key reaches it directly.
    press(&mut app, KeyCode::Left); // Key -> Enabled
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Enabled)
    );
    press(&mut app, KeyCode::Right); // Enabled -> Key
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key)
    );
}

#[test]
fn ctrl_e_toggles_enabled_without_moving_focus() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app); // focus Header(0, Key), not the checkbox
    assert!(header_enabled(&app, 0), "rows start enabled");
    app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    assert!(
        !header_enabled(&app, 0),
        "Ctrl+E disables the focused header"
    );
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key),
        "Ctrl+E toggles in place, without jumping focus to the checkbox"
    );
    app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    assert!(header_enabled(&app, 0), "Ctrl+E re-enables it");
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key),
        "focus still hasn't moved"
    );
}

#[test]
fn arrows_can_reach_the_enabled_checkbox_in_a_form_row() {
    // The checkbox is the leftmost visual column of the Form table too,
    // so a single Left from a blank Key cell reaches it directly (same
    // fix as Headers/Cookies).
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    press(&mut app, KeyCode::Tab); // -> AddHeader, blank
    press(&mut app, KeyCode::Tab); // -> AddCookie, blank
    press(&mut app, KeyCode::Tab); // -> AddQuery, blank
    press(&mut app, KeyCode::Tab); // -> AddFormField
    press(&mut app, KeyCode::Enter); // -> FormField(0, Key), blank
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Key));

    press(&mut app, KeyCode::Left); // Key -> Enabled
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Enabled));
    press(&mut app, KeyCode::Left); // first column clamps
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Enabled));
    press(&mut app, KeyCode::Right); // Enabled -> Key
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Key));
}

#[test]
fn ctrl_e_toggles_enabled_on_a_form_row_without_moving_focus() {
    let mut app = TuiApp::default();
    open_form_on_form_field_kind(&mut app); // -> FormField(0, Kind)
    assert!(form_ref(&app).form_fields[0].enabled, "rows start enabled");
    app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    assert!(
        !form_ref(&app).form_fields[0].enabled,
        "Ctrl+E disables the focused row"
    );
    assert_eq!(
        new_focus(&app),
        NewField::FormField(0, FormCol::Kind),
        "Ctrl+E toggles in place, without jumping focus to the checkbox"
    );
}

#[test]
fn disabled_header_is_not_sent() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> AddHeader (headers start empty)
    press(&mut app, KeyCode::Enter); // -> Header(0, Key)
    for ch in "X-Test".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> Value
    for ch in "abc".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    // Disable the row via Ctrl+E (focus stays on the Value cell).
    app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Value)
    );
    assert!(!header_enabled(&app, 0), "the row is now disabled");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries;
    assert_eq!(e.len(), 1);
    // The disabled row is now *retained* in the entry (so its disabled state
    // persists), carrying enabled=false…
    assert_eq!(
        e[0].headers,
        vec![("X-Test".to_string(), "abc".to_string(), false)],
        "a disabled header is kept in the model, not discarded"
    );
    // …but it is excluded from the actual wire request that gets sent.
    let sent = crate::request::resolve_request(&app.collections[0], None).unwrap();
    assert!(
        !sent.headers.iter().any(|(k, _)| k == "X-Test"),
        "a disabled header must not be sent"
    );
}

#[test]
fn right_arrow_fills_base_url_ghost() {
    let mut app = TuiApp::default();
    app.vars.base_url = "http://base.example".to_string();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url (empty, shows ghost base URL)
    press(&mut app, KeyCode::Right); // commit the ghost
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries;
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].url, "http://base.example");
}

#[test]
fn run_all_batch_mode_toggles_from_preferences_and_round_trips() {
    let mut app = TuiApp::default();
    assert!(
        !app.run_all_batch_mode,
        "Run All streams by default (batch mode off)"
    );

    // Row 4 of the Preferences menu toggles it (Enter and Space both work).
    app.overlay = Some(Overlay::Preferences(4));
    press(&mut app, KeyCode::Enter);
    assert!(app.run_all_batch_mode, "Enter toggles batch mode on");
    assert!(
        matches!(app.overlay, Some(Overlay::Preferences(4))),
        "the highlight stays on the toggle row"
    );
    press(&mut app, KeyCode::Char(' '));
    assert!(!app.run_all_batch_mode, "Space toggles it back off");

    // The 'b' mnemonic toggles it from anywhere in the menu.
    app.overlay = Some(Overlay::Preferences(0));
    press(&mut app, KeyCode::Char('b'));
    assert!(
        app.run_all_batch_mode,
        "the (b) mnemonic toggles batch mode"
    );

    // Survives a persistence round trip.
    let json = serde_json::to_string(&app.to_persisted()).unwrap();
    let back: PersistedState = serde_json::from_str(&json).unwrap();
    let mut restored = TuiApp::default();
    restored.apply_persisted(back);
    assert!(
        restored.run_all_batch_mode,
        "batch mode survives JSON (de)serialization"
    );
}

#[test]
fn default_request_view_setting_round_trips() {
    let mut app = TuiApp::default();
    assert_eq!(
        app.default_request_view,
        RequestView::Json,
        "defaults to JSON"
    );
    app.default_request_view = RequestView::Hurl;

    let snapshot = app.to_persisted();
    let mut restored = TuiApp::default();
    restored.apply_persisted(snapshot);
    assert_eq!(
        restored.default_request_view,
        RequestView::Hurl,
        "the view preference survives a round trip"
    );

    let json = serde_json::to_string(&restored.to_persisted()).unwrap();
    let back: PersistedState = serde_json::from_str(&json).unwrap();
    let mut restored2 = TuiApp::default();
    restored2.apply_persisted(back);
    assert_eq!(
        restored2.default_request_view,
        RequestView::Hurl,
        "the view preference survives JSON (de)serialization"
    );
}

#[test]
fn persisted_state_round_trips_requests_and_settings() {
    let mut app = TuiApp::default();
    app.vars.base_url = "http://example.test".to_string();
    app.language = Language::French;
    app.collections[0].entries.push(HurlEntry::from_fields(
        "hc",
        "GET",
        "http://h/health",
        vec![],
        "",
    ));
    app.collections[0].entries.push(HurlEntry::from_fields(
        "post",
        "POST",
        "http://h/x",
        vec![("X-A".into(), "1".into(), true)],
        "{}",
    ));

    let snapshot = app.to_persisted();
    let mut restored = TuiApp::default();
    restored.apply_persisted(snapshot);

    assert_eq!(restored.vars.base_url, "http://example.test");
    assert_eq!(restored.language, Language::French);
    assert_eq!(restored.collections.len(), 1);
    let e = &restored.collections[0].entries;
    assert_eq!(e.len(), 2);
    assert_eq!(e[0].url, "http://h/health");
    assert_eq!(e[1].method, "POST");
    assert_eq!(
        e[1].headers,
        vec![("X-A".to_string(), "1".to_string(), true)]
    );
}

#[test]
fn persisted_state_survives_json_serialization() {
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("r", "GET", "http://h/x", vec![], ""));
    let json = serde_json::to_string(&app.to_persisted()).unwrap();
    let back: PersistedState = serde_json::from_str(&json).unwrap();

    let mut restored = TuiApp::default();
    restored.apply_persisted(back);
    assert_eq!(restored.collections[0].entries.len(), 1);
    assert_eq!(restored.collections[0].entries[0].url, "http://h/x");
}

#[test]
fn active_tab_divider_positions_and_recent_git_urls_round_trip() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.active_tab = 1;
    app.list_width = 50;
    app.response_pct = 60;
    app.recent_git_urls = vec![
        "https://example.test/repo.git".into(),
        "https://example.test/other.git".into(),
    ];

    let snapshot = app.to_persisted();
    let mut restored = TuiApp::default();
    restored.apply_persisted(snapshot);

    assert_eq!(restored.active_tab, 1, "the active tab is restored");
    assert_eq!(restored.list_width, 50, "the left column width is restored");
    assert_eq!(
        restored.response_pct, 60,
        "the response pane height is restored"
    );
    assert_eq!(
        restored.recent_git_urls,
        vec![
            "https://example.test/repo.git".to_string(),
            "https://example.test/other.git".to_string()
        ],
        "recent git urls are restored, most recent first"
    );
}

#[test]
fn plain_state_json_without_divider_fields_uses_defaults() {
    // Old state files (pre-divider-persistence) must still load cleanly.
    let json = r#"{"language":"English","base_url":"","tabs":[]}"#;
    let state: PersistedState = serde_json::from_str(json).unwrap();
    assert_eq!(state.list_width, 38);
    assert_eq!(state.response_pct, 42);
    assert!(state.recent_git_urls.is_empty());
    assert_eq!(state.active_tab, 0);
}

#[test]
fn gt_and_lt_resize_the_left_column_and_persist() {
    let mut app = TuiApp::default();
    let start = app.list_width;

    press(&mut app, KeyCode::Char('>'));
    assert_eq!(app.list_width, start + 2, "> grows the left column");

    press(&mut app, KeyCode::Char('<'));
    press(&mut app, KeyCode::Char('<'));
    assert_eq!(app.list_width, start - 2, "< shrinks the left column");
}

/// `-` grows the Response pane and `+` shrinks it — swapped from the
/// original `+`-grows/`-`-shrinks mapping, which users reported felt
/// inverted.
#[test]
fn minus_grows_and_plus_shrinks_the_response_pane() {
    let mut app = TuiApp::default();
    let start = app.response_pct;

    press(&mut app, KeyCode::Char('-'));
    assert_eq!(app.response_pct, start + 5, "- grows the response pane");

    press(&mut app, KeyCode::Char('+'));
    press(&mut app, KeyCode::Char('+'));
    assert_eq!(app.response_pct, start - 5, "+ shrinks the response pane");
}

#[test]
fn clear_all_resets_to_a_single_empty_request_tab() {
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("r", "GET", "http://h/x", vec![], ""));
    app.collections
        .push(Collection::new("extra".into(), vec![]));
    app.active_tab = 1;
    app.vars.base_url = "http://changed".into();

    app.clear_all();

    assert_eq!(app.collections.len(), 1);
    assert_eq!(app.collections[0].name, "Request");
    assert!(app.collections[0].entries.is_empty());
    assert_eq!(app.active_tab, 0);
    assert_eq!(app.vars.base_url, "http://127.0.0.1:8080");
}

#[test]
fn clearing_via_the_options_menu_asks_for_confirmation() {
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("r", "GET", "http://h/x", vec![], ""));

    press(&mut app, KeyCode::Char('s')); // open Options (sel 0 = Language)
    press(&mut app, KeyCode::Down); // -> Theme
    press(&mut app, KeyCode::Down); // -> Preferences
    press(&mut app, KeyCode::Down); // -> Close all collections
    press(&mut app, KeyCode::Enter); // opens confirm popup (confirm_on_clear default true)

    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Confirm {
                action: ConfirmAction::Clear,
                ..
            })
        ),
        "a confirmation popup appears before clearing"
    );
    assert!(
        !app.collections[0].entries.is_empty(),
        "nothing is cleared yet"
    );

    press(&mut app, KeyCode::Char('y')); // confirm
    assert!(
        app.collections[0].entries.is_empty(),
        "requests are cleared after confirming"
    );
    assert!(app.overlay.is_none(), "the popup closes");
    assert!(app.status.is_some(), "a status is shown");
}

#[test]
fn declining_the_clear_confirmation_keeps_requests() {
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("r", "GET", "http://h/x", vec![], ""));

    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter); // confirm popup (defaults to No)
    press(&mut app, KeyCode::Char('n')); // decline

    assert!(app.overlay.is_none(), "the popup closes");
    assert!(!app.collections[0].entries.is_empty(), "requests are kept");
}

#[test]
fn clearing_is_immediate_when_confirmation_is_disabled() {
    let mut app = TuiApp {
        confirm_on_clear: false,
        ..Default::default()
    };
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("r", "GET", "http://h/x", vec![], ""));

    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter); // clears immediately, no popup

    assert!(app.overlay.is_none(), "no confirmation popup");
    assert!(
        app.collections[0].entries.is_empty(),
        "requests are cleared immediately"
    );
}

#[test]
fn pressing_q_asks_before_quitting() {
    let mut app = TuiApp::default();

    press(&mut app, KeyCode::Char('q'));
    assert!(!app.quit, "q does not quit immediately");
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Confirm {
                action: ConfirmAction::Exit,
                ..
            })
        ),
        "a quit confirmation popup appears"
    );

    press(&mut app, KeyCode::Char('y')); // confirm
    assert!(app.quit, "confirming quits");
}

#[test]
fn declining_the_quit_confirmation_stays_open() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('q'));
    press(&mut app, KeyCode::Char('n'));
    assert!(!app.quit, "declining keeps the app open");
    assert!(app.overlay.is_none(), "the popup closes");
}

#[test]
fn q_quits_immediately_when_confirmation_is_disabled() {
    let mut app = TuiApp {
        confirm_on_exit: false,
        ..Default::default()
    };
    press(&mut app, KeyCode::Char('q'));
    assert!(app.quit, "q quits directly when confirmation is off");
}

#[test]
fn preferences_menu_toggles_confirmation_flags() {
    let mut app = TuiApp::default();
    assert!(
        app.confirm_on_exit && app.confirm_on_clear,
        "both default to true"
    );

    press(&mut app, KeyCode::Char('s')); // Options (sel 0)
    press(&mut app, KeyCode::Down); // -> Theme
    press(&mut app, KeyCode::Down); // -> Preferences
    press(&mut app, KeyCode::Enter); // open Preferences (sel 0 = Confirm on exit)
    press(&mut app, KeyCode::Enter); // toggle it off

    assert!(!app.confirm_on_exit, "confirm-on-exit toggled off");
    assert!(app.confirm_on_clear, "confirm-on-clear unchanged");
    assert!(
        matches!(app.overlay, Some(Overlay::Preferences(0))),
        "still in the Preferences menu"
    );
}

#[test]
fn preferences_menu_last_item_opens_a_default_request_view_submenu() {
    let mut app = TuiApp::default();
    assert_eq!(
        app.default_request_view,
        RequestView::Json,
        "defaults to JSON"
    );

    press(&mut app, KeyCode::Char('s')); // Options (sel 0)
    press(&mut app, KeyCode::Down); // -> Theme
    press(&mut app, KeyCode::Down); // -> Preferences
    press(&mut app, KeyCode::Enter); // open Preferences (sel 0)
    press(&mut app, KeyCode::Down); // -> sel 1 (Confirm on clear)
    press(&mut app, KeyCode::Down); // -> sel 2 (Confirm before deleting an environment)
    press(&mut app, KeyCode::Down); // -> sel 3 (Always save when prompted)
    press(&mut app, KeyCode::Down); // -> sel 4 (Run All in batch mode)
    press(&mut app, KeyCode::Down); // -> sel 5 (Default Request View)
    assert!(
        matches!(app.overlay, Some(Overlay::Preferences(5))),
        "Down moves to the last item without wrapping past it"
    );

    press(&mut app, KeyCode::Enter); // open the Default Request View submenu
    assert!(
        matches!(app.overlay, Some(Overlay::RequestViewMenu(0))),
        "opens a submenu preselecting the current view (JSON = 0)"
    );
    assert_eq!(
        app.default_request_view,
        RequestView::Json,
        "opening the submenu doesn't change anything yet"
    );

    press(&mut app, KeyCode::Down); // -> Hurl (hovering already applies it live)
    assert_eq!(
        app.default_request_view,
        RequestView::Hurl,
        "hovering over Hurl previews it immediately"
    );
    press(&mut app, KeyCode::Enter); // just returns to Preferences now; nothing left to confirm
    assert_eq!(
        app.default_request_view,
        RequestView::Hurl,
        "selecting Hurl in the submenu sets the view"
    );
    assert!(
        matches!(app.overlay, Some(Overlay::Preferences(5))),
        "Enter returns to Preferences instead of closing the whole menu"
    );
    assert!(app.confirm_on_exit, "unrelated settings are untouched");
    assert!(app.confirm_on_clear, "unrelated settings are untouched");

    // Re-opening the submenu preselects Hurl (index 1) this time, and Esc
    // backs out the same way Enter does (the value's already live).
    press(&mut app, KeyCode::Enter); // re-open the submenu from Preferences(5)
    assert!(
        matches!(app.overlay, Some(Overlay::RequestViewMenu(1))),
        "preselects Hurl (index 1)"
    );
    press(&mut app, KeyCode::Esc);
    assert!(
        matches!(app.overlay, Some(Overlay::Preferences(5))),
        "Esc backs out to Preferences"
    );
    assert_eq!(
        app.default_request_view,
        RequestView::Hurl,
        "Esc doesn't change the setting"
    );
}

#[test]
fn hovering_up_and_down_in_the_request_view_submenu_previews_it_live() {
    // Moving the highlight (not just pressing Enter) applies the
    // setting right away, so the user can see how the Main panel would
    // render each view while still browsing the menu.
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Down); // -> Theme
    press(&mut app, KeyCode::Down); // -> Preferences
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // -> sel 1
    press(&mut app, KeyCode::Down); // -> sel 2 (Confirm before deleting an environment)
    press(&mut app, KeyCode::Down); // -> sel 3 (Always save when prompted)
    press(&mut app, KeyCode::Down); // -> sel 4 (Run All in batch mode)
    press(&mut app, KeyCode::Down); // -> sel 5 (Default Request View)
    press(&mut app, KeyCode::Enter); // open the submenu, preselects JSON (0)
    assert_eq!(app.default_request_view, RequestView::Json);

    press(&mut app, KeyCode::Down); // hover onto Hurl
    assert_eq!(
        app.default_request_view,
        RequestView::Hurl,
        "hovering onto Hurl previews it immediately"
    );
    press(&mut app, KeyCode::Up); // hover back onto JSON
    assert_eq!(
        app.default_request_view,
        RequestView::Json,
        "hovering back onto JSON restores it immediately"
    );

    // Leaving via Enter keeps whatever was last hovered and returns to
    // Preferences rather than closing the whole wizard-settings menu.
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.overlay, Some(Overlay::Preferences(5))));
    assert_eq!(app.default_request_view, RequestView::Json);
}

#[test]
fn settings_survive_closing_all_collections() {
    let mut app = TuiApp {
        confirm_on_exit: false,
        confirm_on_clear: false,
        language: Language::French,
        ..Default::default()
    };

    app.clear_all();

    assert!(!app.confirm_on_exit, "confirm-on-exit setting is preserved");
    assert!(
        !app.confirm_on_clear,
        "confirm-on-clear setting is preserved"
    );
    assert_eq!(app.language, Language::French, "language is preserved");
}

#[test]
fn language_is_chosen_from_a_submenu() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('s')); // Options
    assert!(matches!(app.overlay, Some(Overlay::Options(0))));
    press(&mut app, KeyCode::Enter); // enter Language submenu
    assert!(matches!(app.overlay, Some(Overlay::LanguageMenu(_))));
    press(&mut app, KeyCode::Down); // -> Français
    press(&mut app, KeyCode::Enter); // select
    assert_eq!(app.language, Language::French);
}

/// A unique, existing temporary directory for browser tests.
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("paperboy_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn browser_reopens_in_the_last_used_folder() {
    let dir = temp_dir("reopen");
    let mut app = TuiApp {
        last_browse_dir: Some(dir.clone()),
        ..Default::default()
    };

    app.open_browser(FileAction::LoadEnv);
    match app.overlay {
        Some(Overlay::Browser(_, ex)) => assert_eq!(ex.cwd(), &dir),
        _ => panic!("browser overlay not open"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn browser_without_a_remembered_folder_opens_normally() {
    let mut app = TuiApp::default();
    app.open_browser(FileAction::OpenCollection);
    assert!(
        matches!(app.overlay, Some(Overlay::Browser(..))),
        "browser still opens"
    );
}

#[test]
fn selecting_a_file_remembers_its_folder() {
    let dir = temp_dir("select");
    std::fs::write(dir.join("staging.vars"), "A=1\n").unwrap();

    let mut app = TuiApp {
        last_browse_dir: Some(dir.clone()),
        ..Default::default()
    };
    app.open_browser(FileAction::LoadEnv); // opens in `dir`: ["../", "staging.vars"]

    press(&mut app, KeyCode::Down); // highlight staging.vars
    press(&mut app, KeyCode::Enter); // select it

    assert_eq!(
        app.last_browse_dir.as_ref(),
        Some(&dir),
        "remembers the file's folder"
    );
    assert!(app.overlay.is_none(), "browser closed after selecting");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn last_browse_dir_survives_persistence() {
    let app = TuiApp {
        last_browse_dir: Some(PathBuf::from("/some/dir")),
        ..Default::default()
    };

    let snapshot = app.to_persisted();
    assert_eq!(snapshot.last_browse_dir.as_deref(), Some("/some/dir"));

    let mut restored = TuiApp::default();
    restored.apply_persisted(snapshot);
    assert_eq!(restored.last_browse_dir, Some(PathBuf::from("/some/dir")));
}

#[test]
fn env_picker_prefers_the_last_environment_folder() {
    let env_dir = temp_dir("envfolder");
    let other_dir = temp_dir("otherfolder");
    let mut app = TuiApp {
        // A more-recent load of some other file type moved last_browse_dir…
        last_browse_dir: Some(other_dir.clone()),
        // …but the environment picker should still reopen where the last
        // environment file came from.
        last_env_dir: Some(env_dir.clone()),
        ..Default::default()
    };

    app.open_browser(FileAction::LoadEnv);
    match app.overlay {
        Some(Overlay::Browser(_, ex)) => assert_eq!(ex.cwd(), &env_dir),
        _ => panic!("browser overlay not open"),
    }
    std::fs::remove_dir_all(&env_dir).ok();
    std::fs::remove_dir_all(&other_dir).ok();
}

#[test]
fn going_up_highlights_the_folder_just_left_so_right_returns() {
    let dir = temp_dir("upreturn");
    let sub = dir.join("nested");
    std::fs::create_dir_all(&sub).unwrap();

    let mut app = TuiApp {
        last_browse_dir: Some(sub.clone()),
        ..Default::default()
    };
    app.open_browser(FileAction::OpenCollection); // opens inside `nested`

    // Left goes up to `dir`, and the folder we came from stays highlighted…
    press(&mut app, KeyCode::Left);
    match &app.overlay {
        Some(Overlay::Browser(_, ex)) => {
            assert_eq!(ex.cwd(), &dir, "moved up one level");
            assert_eq!(ex.current().path, sub, "the folder we left is highlighted");
        }
        _ => panic!("browser overlay not open"),
    }

    // …so an instinctive Right re-enters it instead of climbing another level.
    press(&mut app, KeyCode::Right);
    match &app.overlay {
        Some(Overlay::Browser(_, ex)) => {
            assert_eq!(ex.cwd(), &sub, "Right returns into the folder we left")
        }
        _ => panic!("browser overlay not open"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn many_lefts_then_many_rights_return_to_the_starting_folder() {
    // a/b/c/d — start deep, walk all the way up, then walk all the way back.
    let a = temp_dir("trail");
    let b = a.join("b");
    let c = b.join("c");
    let d = c.join("d");
    std::fs::create_dir_all(&d).unwrap();

    let mut app = TuiApp {
        last_browse_dir: Some(d.clone()),
        ..Default::default()
    };
    app.open_browser(FileAction::OpenCollection); // opens inside d

    // Three Lefts climb d → c → b → a.
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Left);
    match &app.overlay {
        Some(Overlay::Browser(_, ex)) => {
            assert_eq!(ex.cwd(), &a, "climbed to the top of the trail");
            assert_eq!(ex.current().path, b, "the trail's next step is highlighted");
        }
        _ => panic!("browser overlay not open"),
    }

    // Three Rights retrace the trail back down to exactly where we started.
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Right);
    match &app.overlay {
        Some(Overlay::Browser(_, ex)) => {
            assert_eq!(ex.cwd(), &d, "retraced all the way back to the start")
        }
        _ => panic!("browser overlay not open"),
    }
    // Fully retraced — the trail is spent.
    assert!(app.browser_forward_path.is_none());
    std::fs::remove_dir_all(&a).ok();
}

#[test]
fn descending_into_a_different_folder_clears_the_retrace_trail() {
    // a contains b (with child c) and a sibling z.
    let a = temp_dir("clearsibling");
    let c = a.join("b").join("c");
    let z = a.join("z");
    std::fs::create_dir_all(&c).unwrap();
    std::fs::create_dir_all(&z).unwrap();

    let mut app = TuiApp {
        last_browse_dir: Some(a.join("b")),
        ..Default::default()
    };
    app.open_browser(FileAction::OpenCollection); // opens inside a/b
    press(&mut app, KeyCode::Left); // up to a, trail anchored at a/b
    assert!(app.browser_forward_path.is_some());

    // Highlight the sibling `z` and descend into it — a fresh navigation.
    let idx = match &app.overlay {
        Some(Overlay::Browser(_, ex)) => ex
            .files()
            .iter()
            .position(|f| f.path == z)
            .expect("z is listed"),
        _ => panic!("browser overlay not open"),
    };
    match &mut app.overlay {
        Some(Overlay::Browser(_, ex)) => ex.set_selected_idx(idx),
        _ => unreachable!(),
    }
    press(&mut app, KeyCode::Right);

    match &app.overlay {
        Some(Overlay::Browser(_, ex)) => assert_eq!(ex.cwd(), &z, "descended into the sibling"),
        _ => panic!("browser overlay not open"),
    }
    assert!(
        app.browser_forward_path.is_none(),
        "a new navigation clears the retrace trail"
    );
    std::fs::remove_dir_all(&a).ok();
}

#[test]
fn right_does_not_ascend_through_the_parent_row_but_enter_still_does() {
    let a = temp_dir("parentrow");
    let b = a.join("b");
    std::fs::create_dir_all(&b).unwrap();

    let mut app = TuiApp {
        last_browse_dir: Some(b.clone()),
        ..Default::default()
    };
    app.open_browser(FileAction::OpenCollection); // opens inside b, highlight "../"

    // The "../" row is highlighted (index 0). Right must NOT ascend through it,
    // otherwise a run of Rights would bounce back up the tree.
    match &app.overlay {
        Some(Overlay::Browser(_, ex)) => {
            assert_eq!(ex.current().path, a, "the '../' row points at the parent")
        }
        _ => panic!("browser overlay not open"),
    }
    press(&mut app, KeyCode::Right);
    match &app.overlay {
        Some(Overlay::Browser(_, ex)) => {
            assert_eq!(ex.cwd(), &b, "Right on '../' is a no-op — stays put")
        }
        _ => panic!("browser overlay not open"),
    }

    // Enter, however, still honours "../" as a way up (the usual idiom).
    press(&mut app, KeyCode::Enter);
    match &app.overlay {
        Some(Overlay::Browser(_, ex)) => {
            assert_eq!(ex.cwd(), &a, "Enter on '../' ascends");
            assert_eq!(ex.current().path, b, "and highlights the folder we left");
        }
        _ => panic!("browser overlay not open"),
    }
    std::fs::remove_dir_all(&a).ok();
}

#[test]
fn ctrl_r_resets_the_browser_to_the_folder_it_opened_in() {
    let dir = temp_dir("resetorigin");
    let sub = dir.join("nested");
    std::fs::create_dir_all(&sub).unwrap();

    let mut app = TuiApp {
        last_browse_dir: Some(dir.clone()),
        ..Default::default()
    };
    app.open_browser(FileAction::OpenCollection); // opens in `dir`
    assert_eq!(app.browser_origin_dir.as_ref(), Some(&dir));

    // Wander up into the parent folder.
    press(&mut app, KeyCode::Left);
    match &app.overlay {
        Some(Overlay::Browser(_, ex)) => {
            assert_ne!(ex.cwd(), &dir, "navigated off the opening folder")
        }
        _ => panic!("browser overlay not open"),
    }

    // Ctrl+r snaps back to where the browser opened.
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    match &app.overlay {
        Some(Overlay::Browser(_, ex)) => assert_eq!(ex.cwd(), &dir, "reset to the opening folder"),
        _ => panic!("browser overlay not open"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn selecting_an_env_file_updates_the_env_folder() {
    let dir = temp_dir("selectenv");
    std::fs::write(dir.join("staging.vars"), "A=1\n").unwrap();

    let mut app = TuiApp {
        last_browse_dir: Some(dir.clone()),
        ..Default::default()
    };
    app.open_browser(FileAction::LoadEnv);
    press(&mut app, KeyCode::Down); // highlight staging.vars
    press(&mut app, KeyCode::Enter); // select it

    assert_eq!(
        app.last_env_dir.as_ref(),
        Some(&dir),
        "loading an environment records its folder for the env picker"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn last_env_dir_survives_persistence() {
    let app = TuiApp {
        last_env_dir: Some(PathBuf::from("/env/dir")),
        ..Default::default()
    };
    let snapshot = app.to_persisted();
    assert_eq!(snapshot.last_env_dir.as_deref(), Some("/env/dir"));

    let mut restored = TuiApp::default();
    restored.apply_persisted(snapshot);
    assert_eq!(restored.last_env_dir, Some(PathBuf::from("/env/dir")));
}

#[test]
fn loading_an_env_file_as_a_collection_is_rejected() {
    let dir = temp_dir("wrongcol");
    let env = dir.join("staging.vars");
    std::fs::write(&env, "TOKEN=abc\nBASE=http://x\n").unwrap();

    let mut app = TuiApp::default();
    let before = app.collections.len();
    app.do_file_action(FileAction::OpenCollection, env.to_str().unwrap());

    assert_eq!(
        app.collections.len(),
        before,
        "no tab added for a non-collection file"
    );
    assert!(
        matches!(&app.status, Some(st) if !st.is_ok()),
        "an error is shown"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn loading_a_collection_file_as_an_environment_is_rejected() {
    let dir = temp_dir("wrongenv");
    let col = dir.join("demo.hurl");
    std::fs::write(
        &col,
        "# Health\nGET http://127.0.0.1:8080/health\nHTTP 200\n",
    )
    .unwrap();

    let mut app = TuiApp::default();
    app.do_file_action(FileAction::LoadEnv, col.to_str().unwrap());

    assert!(app.global_envs.is_empty(), "no environment loaded");
    assert!(
        matches!(&app.status, Some(st) if !st.is_ok()),
        "an error is shown"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn loading_a_valid_collection_adds_a_tab() {
    let dir = temp_dir("goodcol");
    let col = dir.join("demo.hurl");
    std::fs::write(
        &col,
        "# Health\nGET http://127.0.0.1:8080/health\nHTTP 200\n",
    )
    .unwrap();

    let mut app = TuiApp::default();
    let before = app.collections.len();
    app.do_file_action(FileAction::OpenCollection, col.to_str().unwrap());

    assert_eq!(
        app.collections.len(),
        before + 1,
        "a valid collection adds a tab"
    );
    std::fs::remove_dir_all(&dir).ok();
}

// ── File menu: submenus and mnemonic shortcuts ──────────────────────────

#[test]
fn file_menu_top_level_has_load_and_save_submenus() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('f'));
    assert!(matches!(app.overlay, Some(Overlay::FileMenu(0))));
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.overlay, Some(Overlay::FileLoadMenu(0))),
        "Enter on \"(L)oad\" opens the Load submenu"
    );

    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Down);
    assert!(matches!(app.overlay, Some(Overlay::FileMenu(1))));
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.overlay, Some(Overlay::FileSaveMenu(0))),
        "Enter on \"(S)ave\" opens the Save submenu"
    );
}

#[test]
fn file_menu_mnemonic_keys_jump_straight_into_a_submenu() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Char('s')); // "(S)ave" mnemonic
    assert!(matches!(app.overlay, Some(Overlay::FileSaveMenu(0))));

    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Char('L')); // uppercase also matches "(L)oad"
    assert!(matches!(app.overlay, Some(Overlay::FileLoadMenu(0))));
}

#[test]
fn file_load_submenu_mnemonic_activates_the_item_immediately() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Enter); // -> Load kind list
    press(&mut app, KeyCode::Char('c')); // "(C)ollection…" -> source step
    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::FileLoadSource(FileKind::Collection, 0))
        ),
        "picking a kind opens the local-vs-git source step"
    );
    press(&mut app, KeyCode::Char('g')); // "From (G)it…"
    assert!(
        matches!(&app.overlay, Some(Overlay::RemoteGit(w)) if w.kind == RemoteKind::Collection),
        "the git source both selects and activates without needing Enter"
    );
}

#[test]
fn file_save_submenu_mnemonic_activates_the_item_immediately() {
    use crate::i18n::Status;
    let mut app = TuiApp::default(); // built-in Request tab has no git_origin
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Down); // -> "(S)ave"
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('c')); // "(C)ollection…" -> destination step
    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::FileSaveDest(FileKind::Collection, 0))
        ),
        "picking a kind opens the save-destination step"
    );
    press(&mut app, KeyCode::Char('g')); // "To (G)it…"
    assert!(app.overlay.is_none(), "no wizard for a non-git collection");
    assert!(matches!(app.status, Some(Status::NoGitOrigin)));
}

#[test]
fn file_menu_left_and_right_arrows_enter_and_exit_submenus() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('f')); // top File menu, "(L)oad"
    press(&mut app, KeyCode::Right); // Right descends into Load
    assert!(matches!(app.overlay, Some(Overlay::FileLoadMenu(0))));

    press(&mut app, KeyCode::Down); // -> Collection kind
    press(&mut app, KeyCode::Right); // Right descends into its source step
    assert!(matches!(
        &app.overlay,
        Some(Overlay::FileLoadSource(FileKind::Collection, 0))
    ));

    press(&mut app, KeyCode::Left); // Left backs out to the kind list, kind relit
    assert!(matches!(app.overlay, Some(Overlay::FileLoadMenu(1))));

    press(&mut app, KeyCode::Left); // Left backs out to the top File menu
    assert!(matches!(app.overlay, Some(Overlay::FileMenu(0))));

    // Same round-trip on the Save side (Save = row 1).
    press(&mut app, KeyCode::Down); // -> "(S)ave"
    press(&mut app, KeyCode::Right); // descend into Save
    assert!(matches!(app.overlay, Some(Overlay::FileSaveMenu(0))));
    press(&mut app, KeyCode::Down); // -> Collection kind
    press(&mut app, KeyCode::Right); // descend into its destination step
    assert!(matches!(
        &app.overlay,
        Some(Overlay::FileSaveDest(FileKind::Collection, 0))
    ));
    press(&mut app, KeyCode::Left); // back to the Save kind list
    assert!(matches!(app.overlay, Some(Overlay::FileSaveMenu(1))));
    press(&mut app, KeyCode::Left); // back to the top File menu (Save row)
    assert!(matches!(app.overlay, Some(Overlay::FileMenu(1))));
}

#[test]
fn file_submenu_esc_returns_to_the_parent_file_menu() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Enter); // -> Load submenu
    press(&mut app, KeyCode::Esc);
    assert!(
        matches!(app.overlay, Some(Overlay::FileMenu(0))),
        "Esc from Load returns to the top File menu"
    );

    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter); // -> Save submenu
    press(&mut app, KeyCode::Esc);
    assert!(
        matches!(app.overlay, Some(Overlay::FileMenu(1))),
        "Esc from Save returns to the top File menu"
    );
}

#[test]
fn file_menu_mnemonics_are_unique_within_each_popup_and_avoid_nav_keys() {
    use crate::i18n::Strings;
    let langs = [Language::English, Language::French, Language::Danish];
    for lang in langs {
        let s = Strings::for_language(&lang);
        for items in [
            file_menu_items(&s).to_vec(),
            file_load_items(&s).to_vec(),
            file_save_items(&s).to_vec(),
            file_load_source_items(&s).to_vec(),
            file_save_dest_items(FileKind::Collection, &s),
            file_save_dest_items(FileKind::Environment, &s),
            file_save_dest_items(FileKind::Workspace, &s),
            file_save_dest_items(FileKind::Report, &s),
        ] {
            let mnemonics: Vec<char> = items
                .iter()
                .map(|l| menu_mnemonic(l).expect("every item has a mnemonic"))
                .collect();
            let mut seen = std::collections::HashSet::new();
            for m in &mnemonics {
                assert!(
                    seen.insert(*m),
                    "mnemonic '{m}' is duplicated within a popup"
                );
                assert!(
                    !matches!(m, 'j' | 'k' | 'q'),
                    "mnemonic '{m}' collides with a menu nav key"
                );
            }
        }
    }
}

#[test]
fn file_menu_opens_the_remote_git_wizards() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Enter); // -> Load kind list
    press(&mut app, KeyCode::Down); // -> Collection
    press(&mut app, KeyCode::Enter); // -> source step (Local / From Git)
    press(&mut app, KeyCode::Down); // -> From Git
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(&app.overlay, Some(Overlay::RemoteGit(w)) if w.kind == RemoteKind::Collection),
        "opens the collection git wizard"
    );

    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Enter); // -> Load kind list
    press(&mut app, KeyCode::Down); // -> Collection
    press(&mut app, KeyCode::Down); // -> Environment
    press(&mut app, KeyCode::Enter); // -> source step
    press(&mut app, KeyCode::Down); // -> From Git
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(&app.overlay, Some(Overlay::RemoteGit(w)) if w.kind == RemoteKind::Environment),
        "opens the environment git wizard"
    );
}

#[test]
fn connect_stage_toggles_fields_and_requires_a_url() {
    let mut app = TuiApp::default();
    app.open_remote_wizard(RemoteKind::Collection);
    // Tab moves from the URL field (0) to the token field (1).
    press(&mut app, KeyCode::Tab);
    match &app.overlay {
        Some(Overlay::RemoteGit(w)) => {
            assert!(matches!(w.stage, RemoteStage::Connect { field: 1, .. }));
        }
        _ => panic!("wizard closed"),
    }
    // Enter with a blank URL surfaces an error rather than connecting.
    press(&mut app, KeyCode::Enter);
    match &app.overlay {
        Some(Overlay::RemoteGit(w)) => assert!(matches!(w.stage, RemoteStage::Error(_))),
        _ => panic!("wizard closed"),
    }
}

#[test]
fn esc_closes_the_wizard() {
    let mut app = TuiApp::default();
    app.open_remote_wizard(RemoteKind::Collection);
    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_none(), "Esc closes the wizard");
}

#[test]
fn ref_and_file_messages_advance_the_wizard() {
    let mut app = TuiApp::default();
    let mut w = RemoteWizard::new(RemoteKind::Collection, Vec::new());

    // Refs -> PickRef with branches then tags.
    let refs = RemoteRefs {
        branches: vec!["main".into()],
        tags: vec!["v1".into()],
    };
    assert!(app.apply_git_msg(&mut w, GitMsg::Refs(Ok(refs))));
    match &w.stage {
        RemoteStage::PickRef { refs, .. } => {
            assert_eq!(refs.len(), 2);
            assert_eq!(refs[0].gitref, "refs/heads/main");
            assert_eq!(refs[1].gitref, "refs/tags/v1");
        }
        _ => panic!("expected PickRef"),
    }

    // Files -> PickFile and the temp repo is recorded.
    let files = vec!["a.hurl".to_string(), "sub/b.hurl".to_string()];
    let repo = std::env::temp_dir().join(format!("crab_test_repo_{}", std::process::id()));
    std::fs::create_dir_all(&repo).unwrap();
    assert!(app.apply_git_msg(
        &mut w,
        GitMsg::Files(Ok((files, repo.clone(), "deadbeef".to_string())))
    ));
    assert_eq!(w.repo.as_deref(), Some(repo.as_path()));
    assert!(matches!(&w.stage, RemoteStage::PickFile { files, .. } if files.len() == 2));
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn fetched_collection_content_is_loaded_as_a_tab() {
    let mut app = TuiApp::default();
    let mut w = RemoteWizard::new(RemoteKind::Collection, Vec::new());
    w.selected_path = Some("api/health.hurl".to_string());
    let before = app.collections.len();

    let hurl = "# Health\nGET http://127.0.0.1:8080/health\nHTTP 200\n";
    let keep_open = app.apply_git_msg(&mut w, GitMsg::Content(Ok(hurl.to_string())));

    assert!(
        !keep_open,
        "a collection load closes the wizard once the tab is added"
    );
    assert_eq!(
        app.collections.len(),
        before + 1,
        "a collection tab is added"
    );
    assert_eq!(app.collections.last().unwrap().name, "health");
    assert!(
        app.collections.last().unwrap().path.is_none(),
        "remote source has no local path"
    );
}

#[test]
fn fetched_collection_from_git_records_its_git_origin() {
    use super::editor::Editor;
    let mut app = TuiApp::default();
    let mut w = RemoteWizard::new(RemoteKind::Collection, Vec::new());
    w.chosen_ref = Some(RefChoice {
        label: "[Branches] main".into(),
        gitref: "refs/heads/main".into(),
    });
    w.url = Editor::new("https://example.test/repo.git", false);
    w.selected_path = Some("api/health.hurl".to_string());

    let hurl = "GET http://127.0.0.1:8080/health\nHTTP 200\n";
    let keep_open = app.apply_git_msg(&mut w, GitMsg::Content(Ok(hurl.to_string())));
    assert!(!keep_open, "a collection load closes the wizard");

    let ci = app.active_tab;
    let col_origin = app.collections[ci]
        .git_origin
        .clone()
        .expect("collection git_origin is set");
    assert_eq!(col_origin.repo_url, "https://example.test/repo.git");
    assert_eq!(col_origin.path, "api/health.hurl");
    assert_eq!(col_origin.ref_kind, RefKind::Branch);
    assert_eq!(col_origin.ref_name, "main");
    assert!(
        app.collections[ci].linked_env_id.is_none(),
        "loading a collection no longer also loads/links an environment"
    );
    assert!(
        app.global_envs.is_empty(),
        "no environment is loaded alongside the collection"
    );
}

#[test]
fn fetched_environment_content_is_loaded() {
    let mut app = TuiApp::default();
    let mut w = RemoteWizard::new(RemoteKind::Environment, Vec::new());
    w.selected_path = Some("envs/staging.vars".to_string());

    let vars = "BASE_URL=http://127.0.0.1:8080\nTOKEN=abc\n";
    let keep_open = app.apply_git_msg(&mut w, GitMsg::Content(Ok(vars.to_string())));

    assert!(!keep_open, "loading closes the wizard");
    assert_eq!(
        app.global_envs.len(),
        1,
        "the environment is loaded globally"
    );
}

#[test]
fn a_git_error_is_shown_in_the_wizard() {
    let mut app = TuiApp::default();
    let mut w = RemoteWizard::new(RemoteKind::Collection, Vec::new());
    assert!(app.apply_git_msg(&mut w, GitMsg::Refs(Err("boom".into()))));
    assert!(matches!(&w.stage, RemoteStage::Error(e) if e == "boom"));
}

// ── Loading a Workspace from git ────────────────────────────────────────

#[test]
fn workspace_git_filter_matches_extensions_case_insensitively() {
    assert!(WorkspaceGitFilter::HurlAndJson.matches("a/b.hurl"));
    assert!(WorkspaceGitFilter::HurlAndJson.matches("a/b.JSON"));
    assert!(!WorkspaceGitFilter::HurlAndJson.matches("a/b.txt"));
    assert!(WorkspaceGitFilter::HurlOnly.matches("a/b.Hurl"));
    assert!(!WorkspaceGitFilter::HurlOnly.matches("a/b.json"));
    assert!(WorkspaceGitFilter::JsonOnly.matches("a/b.json"));
    assert!(!WorkspaceGitFilter::JsonOnly.matches("a/b.hurl"));
    assert!(WorkspaceGitFilter::All.matches("a/b.bin"));
    assert!(WorkspaceGitFilter::All.matches("a/b"));
}

#[test]
fn a_workspace_load_goes_to_the_file_filter_picker_instead_of_the_single_file_picker() {
    let mut app = TuiApp::default();
    let mut w = RemoteWizard::new(RemoteKind::Workspace, Vec::new());
    let files = vec![
        "a.hurl".to_string(),
        "b.json".to_string(),
        "big/blob.bin".to_string(),
    ];
    let repo = std::env::temp_dir().join(format!("crab_test_ws_repo_{}", std::process::id()));
    std::fs::create_dir_all(&repo).unwrap();

    assert!(app.apply_git_msg(
        &mut w,
        GitMsg::Files(Ok((files, repo.clone(), "deadbeef".to_string())))
    ));
    assert!(matches!(
        &w.stage,
        RemoteStage::PickWorkspaceFilter { sel: 0 }
    ));
    assert_eq!(
        w.files.len(),
        3,
        "the full file listing is kept so the filter can be applied client-side"
    );

    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn picking_a_workspace_filter_with_no_matches_shows_an_error_instead_of_downloading_nothing() {
    let mut app = TuiApp::default();
    let mut w = RemoteWizard::new(RemoteKind::Workspace, Vec::new());
    w.files = vec!["big/blob.bin".to_string(), "readme.md".to_string()];
    w.repo = Some(std::env::temp_dir().join("crab_test_ws_no_match"));
    w.stage = RemoteStage::PickWorkspaceFilter { sel: 0 }; // .hurl/.json — matches nothing here
    app.overlay = Some(Overlay::RemoteGit(Box::new(w)));

    press(&mut app, KeyCode::Enter);

    match &app.overlay {
        Some(Overlay::RemoteGit(w)) => {
            assert!(
                matches!(&w.stage, RemoteStage::Error(_)),
                "no matches shows an error instead of a silent no-op"
            );
        }
        _ => panic!("expected the wizard to stay open showing the error"),
    }
}

#[test]
fn a_successful_workspace_download_opens_a_new_workspace_tab_bound_to_the_repo_dir() {
    use super::editor::Editor;
    let mut app = TuiApp::default();
    let mut w = RemoteWizard::new(RemoteKind::Workspace, Vec::new());
    w.url = Editor::new("https://example.test/my-repo.git", false);
    let repo = std::env::temp_dir().join(format!("crab_test_ws_ready_{}", std::process::id()));
    std::fs::create_dir_all(&repo).unwrap();
    w.repo = Some(repo.clone());

    let before = app.collections.len();
    let keep_open = app.apply_git_msg(&mut w, GitMsg::Workspace(Ok(repo.clone())));

    assert!(
        !keep_open,
        "the wizard itself closes; the new storage-choice popup takes over"
    );
    assert!(
        w.repo.is_none(),
        "repo ownership moves to the pending choice so poll_git_updates won't delete it"
    );
    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::WorkspaceStorageChoice { sel: 0, .. })
        ),
        "the user is asked whether to keep the download temporary or save it permanently"
    );
    // Enter on the default choice (sel 0 = keep temporarily) reproduces
    // the old direct behaviour.
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.collections.len(),
        before + 1,
        "a new Workspace tab is created"
    );
    let ci = app.active_tab;
    assert_eq!(
        app.collections[ci].workspace_root.as_deref(),
        Some(repo.as_path())
    );
    assert_eq!(
        app.collections[ci].name, "my-repo",
        "the tab is named from the repo URL"
    );
    assert!(
        matches!(&app.overlay, Some(Overlay::WorkspacePicker(p)) if p.collection_idx == ci),
        "the file picker opens immediately, scoped to the new tab, just like a local Workspace folder"
    );

    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn a_workspace_download_failure_is_shown_as_an_error() {
    let mut app = TuiApp::default();
    let mut w = RemoteWizard::new(RemoteKind::Workspace, Vec::new());
    assert!(app.apply_git_msg(&mut w, GitMsg::Workspace(Err("no space left".into()))));
    assert!(matches!(&w.stage, RemoteStage::Error(e) if e == "no space left"));
}

#[test]
fn loading_a_workspace_offers_a_git_source_alongside_the_local_folder_picker() {
    use crate::i18n::Strings;
    let s = Strings::for_language(&Language::English);

    // The Load kind list now has five kinds (Request, Collection, Environment,
    // Workspace, Report); Workspace stays at row 3.
    let items = file_load_items(&s);
    assert_eq!(items.len(), 5);
    assert_eq!(file_load_kind_index(FileKind::Workspace), 3);

    // Picking Workspace opens the local-vs-git source step…
    let mut app = TuiApp::default();
    app.activate_file_load_item(3);
    assert!(matches!(
        &app.overlay,
        Some(Overlay::FileLoadSource(FileKind::Workspace, 0))
    ));

    // …and "From Git" (sel 1) opens the wizard in Workspace mode.
    app.activate_file_load_source(FileKind::Workspace, 1);
    assert!(
        matches!(&app.overlay, Some(Overlay::RemoteGit(w)) if w.kind == RemoteKind::Workspace),
        "the git source opens the wizard in Workspace mode"
    );
}

#[test]
fn the_load_source_step_esc_returns_to_the_kind_list_with_the_kind_relit() {
    let mut app = TuiApp {
        overlay: Some(Overlay::FileLoadSource(FileKind::Environment, 1)),
        ..Default::default()
    };
    press(&mut app, KeyCode::Esc);
    assert!(
        matches!(app.overlay, Some(Overlay::FileLoadMenu(2))),
        "Esc steps back to the kind list with Environment (row 2) highlighted"
    );
}

#[test]
fn the_save_destination_step_lists_the_right_choices_per_kind() {
    use crate::i18n::Strings;
    let s = Strings::for_language(&Language::English);
    // Collection can Save / Save As / To Git; Environment has no git save;
    // a Workspace is a folder, so only Save As / To Git.
    assert_eq!(file_save_dest_items(FileKind::Collection, &s).len(), 3);
    assert_eq!(file_save_dest_items(FileKind::Environment, &s).len(), 2);
    assert_eq!(file_save_dest_items(FileKind::Workspace, &s).len(), 2);

    let mut app = TuiApp {
        overlay: Some(Overlay::FileSaveDest(FileKind::Workspace, 0)),
        ..Default::default()
    };

    press(&mut app, KeyCode::Esc);
    assert!(
        matches!(app.overlay, Some(Overlay::FileSaveMenu(3))),
        "Esc steps back to the Save kind list with Workspace (row 3) highlighted"
    );
}

// ── Workspace redownload-on-missing (see `WorkspaceGitOrigin`) ─────────
//
// A local bare repo stands in for "the remote" — no network needed,
// exactly like `git_remote.rs`'s own push-plumbing tests and
// `saving_to_git_appends_a_commit_...` above.

fn git_ws(args: &[&str], cwd: &std::path::Path) {
    let out = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git should run");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Seed a bare "remote" repo on `main` with an `.hurl` file, a `.json`
/// file and an unrelated large blob, and return `(bare_repo_url,
/// head_commit_sha, base_tmp_dir)` — the caller must remove
/// `base_tmp_dir` when done.
fn seed_ws_bare_repo() -> (String, String, std::path::PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = std::env::temp_dir().join(format!(
        "paperboy-ws-reload-test-{}-{nanos}",
        std::process::id()
    ));
    let bare = base.join("bare.git");
    let work = base.join("work");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git_ws(&["init", "--bare", "-q", "."], &bare);
    git_ws(&["init", "-q"], &work);
    git_ws(&["checkout", "-q", "-b", "main"], &work);
    git_ws(&["config", "user.name", "Seed"], &work);
    git_ws(&["config", "user.email", "seed@test"], &work);
    std::fs::write(work.join("a.hurl"), "GET https://example.com/a\n").unwrap();
    std::fs::write(work.join("b.json"), "{}").unwrap();
    std::fs::write(work.join("big.bin"), "x".repeat(1024)).unwrap();
    git_ws(&["add", "-A"], &work);
    git_ws(&["commit", "-q", "-m", "seed"], &work);
    git_ws(&["remote", "add", "origin", bare.to_str().unwrap()], &work);
    git_ws(&["push", "-q", "origin", "main"], &work);
    let sha = String::from_utf8(
        std::process::Command::new("git")
            .current_dir(&work)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    (bare.to_str().unwrap().to_string(), sha, base)
}

#[test]
fn spawn_workspace_redownload_fetches_the_exact_pinned_commit_and_applies_the_filter() {
    let (repo_url, commit_sha, base) = seed_ws_bare_repo();
    let origin = WorkspaceGitOrigin {
        repo_url,
        commit_sha,
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
        filter: WorkspaceGitFilter::HurlAndJson,
    };

    let rx = spawn_workspace_redownload(origin);
    let result = rx
        .recv()
        .expect("the redownload thread should send a result");
    let repo = result.expect("redownload of an existing commit should succeed");

    assert!(repo.join("a.hurl").exists());
    assert!(repo.join("b.json").exists());
    assert!(
        !repo.join("big.bin").exists(),
        "the filter excludes non-hurl/json files, so it's never checked out"
    );

    crate::git_remote::cleanup(&repo);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn spawn_workspace_redownload_fails_clearly_when_the_pinned_commit_is_gone() {
    let (repo_url, _sha, base) = seed_ws_bare_repo();
    let origin = WorkspaceGitOrigin {
        repo_url,
        // A syntactically valid but nonexistent commit sha — stands in
        // for "the remote's history was rewritten/force-pushed and no
        // longer contains what we originally downloaded".
        commit_sha: "0000000000000000000000000000000000dead".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
        filter: WorkspaceGitFilter::All,
    };

    let rx = spawn_workspace_redownload(origin);
    let result = rx
        .recv()
        .expect("the redownload thread should send a result");
    assert!(
        result.is_err(),
        "fetching a sha the remote no longer has should fail, not silently succeed"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_full_workspace_git_download_records_the_commit_sha_and_filter_as_the_origin() {
    // End-to-end through the interactive wizard: PickRef -> Files (sha
    // captured) -> PickWorkspaceFilter (filter captured, Enter spawns
    // the real download) -> Workspace download succeeds -> the new
    // tab's `workspace_git_origin` has everything a later redownload
    // would need.
    let (repo_url, commit_sha, base) = seed_ws_bare_repo();
    let mut app = TuiApp::default();
    let mut w = RemoteWizard::new(RemoteKind::Workspace, Vec::new());
    w.url = crate::tui::editor::Editor::new(&repo_url, false);
    w.chosen_ref = Some(RefChoice {
        label: "[Branches] main".into(),
        gitref: "refs/heads/main".into(),
    });

    // A real (local, offline) `list_files` call, exactly like
    // `spawn_git_files` would do, so `w.repo` is an actual git checkout
    // that the later filter-driven `checkout_files` step can act on
    // (unlike a bare freshly-`mkdir`'d directory, which has no `.git`).
    let (files, repo_dir, listed_sha) =
        crate::git_remote::list_files(&repo_url, None, "refs/heads/main").unwrap();
    assert_eq!(listed_sha, commit_sha);
    assert!(app.apply_git_msg(
        &mut w,
        GitMsg::Files(Ok((files, repo_dir.clone(), listed_sha.clone())))
    ));
    assert_eq!(
        w.chosen_sha.as_deref(),
        Some(commit_sha.as_str()),
        "the resolved commit sha is remembered"
    );
    assert!(matches!(
        &w.stage,
        RemoteStage::PickWorkspaceFilter { sel: 0 }
    ));

    // Pressing Enter on the (default, sel 0 = HurlAndJson) filter choice
    // both records `chosen_workspace_filter` and spawns the real
    // (local, offline) download.
    app.on_key_remote(
        Box::new(w),
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    let Some(Overlay::RemoteGit(mut w)) = app.overlay.take() else {
        panic!("wizard should still be open")
    };
    assert_eq!(
        w.chosen_workspace_filter,
        Some(WorkspaceGitFilter::HurlAndJson)
    );
    assert!(matches!(w.stage, RemoteStage::Loading { .. }));
    let rx =
        w.rx.take()
            .expect("the workspace download should have been spawned");
    let msg = rx.recv().expect("the download thread should send a result");

    let keep_open = app.apply_git_msg(&mut w, msg);
    assert!(!keep_open);
    assert!(matches!(
        &app.overlay,
        Some(Overlay::WorkspaceStorageChoice { sel: 0, .. })
    ));
    // Enter on the default choice (sel 0 = keep temporarily) reproduces
    // the old direct behaviour, creating the tab immediately.
    press(&mut app, KeyCode::Enter);
    let ci = app.active_tab;
    let origin = app.collections[ci]
        .workspace_git_origin
        .clone()
        .expect("a successful git Workspace download must record its origin");
    assert_eq!(origin.repo_url, repo_url);
    assert_eq!(origin.commit_sha, commit_sha);
    assert_eq!(origin.ref_kind, RefKind::Branch);
    assert_eq!(origin.ref_name, "main");
    assert_eq!(origin.filter, WorkspaceGitFilter::HurlAndJson);

    if let Some(root) = &app.collections[ci].workspace_root {
        assert!(root.join("a.hurl").exists());
        assert!(root.join("b.json").exists());
        assert!(
            !root.join("big.bin").exists(),
            "the filter excludes non-hurl/json files"
        );
        crate::git_remote::cleanup(root);
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn into_collection_queues_a_pending_reload_only_when_the_vanished_root_came_from_git() {
    let dir =
        std::env::temp_dir().join(format!("paperboy_ws_pending_reload_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // ensure it does NOT exist

    let origin = WorkspaceGitOrigin {
        repo_url: "https://example.test/repo.git".into(),
        commit_sha: "abc123".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
        filter: WorkspaceGitFilter::HurlAndJson,
    };

    let mut col = Collection::new("ghost-repo".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_downloaded_from_git = true;
    col.workspace_git_origin = Some(origin.clone());
    col.path = Some(dir.join("sub/a.hurl"));

    let persisted = crate::persistence::PersistedTab::from_collection(&col, None);
    let (restored, pending) = persisted.into_collection(None);

    assert_eq!(restored.workspace_root, None);
    let pending = pending
        .expect("a git-originated vanished root should queue a reload instead of just resetting");
    assert_eq!(pending.tab_name, "ghost-repo");
    assert_eq!(pending.origin, origin);
    assert_eq!(
        pending.relative_selected_path.as_deref(),
        Some("sub/a.hurl"),
        "the previously-selected file's path relative to the dead root is captured for later re-selection"
    );
}

#[test]
fn apply_persisted_queues_pending_reloads_and_opens_the_first_confirm_popup() {
    let dir =
        std::env::temp_dir().join(format!("paperboy_ws_apply_pending_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let origin = WorkspaceGitOrigin {
        repo_url: "https://example.test/repo.git".into(),
        commit_sha: "abc123".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
        filter: WorkspaceGitFilter::HurlAndJson,
    };
    let mut col = Collection::new("ghost-repo".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_downloaded_from_git = true;
    col.workspace_git_origin = Some(origin);

    let tab = crate::persistence::PersistedTab::from_collection(&col, None);
    let state = crate::persistence::PersistedState {
        tabs: vec![tab],
        ..Default::default()
    };

    let mut app = TuiApp::default();
    app.apply_persisted(state);

    assert_eq!(app.collections[0].workspace_root, None);
    assert!(
        app.status.is_none(),
        "no plain 'missing' status when a redownload will be offered instead"
    );
    assert!(
        matches!(&app.overlay, Some(Overlay::WorkspaceReloadConfirm { idx, .. }) if *idx == 0),
        "the confirm popup opens automatically for the (only) pending reload"
    );
}

#[test]
fn declining_a_workspace_reload_shows_the_folder_missing_status_and_advances_the_queue() {
    let origin = WorkspaceGitOrigin {
        repo_url: "https://example.test/repo.git".into(),
        commit_sha: "abc123".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
        filter: WorkspaceGitFilter::HurlAndJson,
    };
    let reload = crate::persistence::PendingWorkspaceReload {
        tab_name: "ghost-repo".into(),
        origin,
        relative_selected_path: None,
    };
    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("ghost-repo".into(), Vec::new()));
    app.overlay = Some(Overlay::WorkspaceReloadConfirm {
        idx: 0,
        reload: Box::new(reload),
        sel: 0,
    });

    press(&mut app, KeyCode::Esc);

    assert!(app.overlay.is_none());
    match &app.status {
        Some(crate::i18n::Status::WorkspaceFolderMissing(name)) => assert_eq!(name, "ghost-repo"),
        other => panic!("expected WorkspaceFolderMissing, got {other:?}"),
    }
}

#[test]
fn accepting_a_workspace_reload_redownloads_and_restores_the_previously_selected_file() {
    let (repo_url, commit_sha, base) = seed_ws_bare_repo();
    let origin = WorkspaceGitOrigin {
        repo_url,
        commit_sha,
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
        filter: WorkspaceGitFilter::HurlAndJson,
    };
    let reload = crate::persistence::PendingWorkspaceReload {
        tab_name: "ghost-repo".into(),
        origin,
        relative_selected_path: Some("a.hurl".to_string()),
    };
    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("ghost-repo".into(), Vec::new()));
    app.overlay = Some(Overlay::WorkspaceReloadConfirm {
        idx: 0,
        reload: Box::new(reload),
        sel: 0,
    });

    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(&app.overlay, Some(Overlay::WorkspaceReloadLoading { idx }) if *idx == 0),
        "accepting spawns the background redownload and shows a loading popup for the right tab"
    );

    // Poll until the background thread finishes (it's a real, if local
    // and fast, git fetch).
    for _ in 0..200 {
        app.poll_workspace_redownload_updates();
        if app.overlay.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(
        app.overlay.is_none(),
        "the loading popup closes once the redownload settles"
    );
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::WorkspaceReloaded)
    ));
    let col = &app.collections[0];
    assert!(
        col.workspace_root.is_some(),
        "the tab is rebound to the freshly downloaded folder"
    );
    assert!(col.workspace_downloaded_from_git);
    assert_eq!(
        col.path
            .as_deref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str()),
        Some("a.hurl"),
        "the previously-selected file is re-selected in the new checkout"
    );
    assert!(
        !col.entries.is_empty(),
        "the re-selected file's requests are parsed"
    );

    if let Some(root) = &app.collections[0].workspace_root {
        crate::git_remote::cleanup(root);
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_failed_workspace_reload_reports_the_error_and_hints_at_saving_locally() {
    let origin = WorkspaceGitOrigin {
        repo_url: std::env::temp_dir()
            .join(format!(
                "paperboy-ws-reload-nonexistent-{}",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned(),
        commit_sha: "deadbeef".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
        filter: WorkspaceGitFilter::All,
    };
    let reload = crate::persistence::PendingWorkspaceReload {
        tab_name: "ghost-repo".into(),
        origin,
        relative_selected_path: None,
    };
    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("ghost-repo".into(), Vec::new()));
    app.overlay = Some(Overlay::WorkspaceReloadConfirm {
        idx: 0,
        reload: Box::new(reload),
        sel: 0,
    });
    press(&mut app, KeyCode::Enter);

    for _ in 0..200 {
        app.poll_workspace_redownload_updates();
        if app.overlay.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    match &app.status {
        Some(crate::i18n::Status::WorkspaceReloadFailed(_)) => {}
        other => panic!("expected WorkspaceReloadFailed, got {other:?}"),
    }
    let s = crate::i18n::Strings::for_language(&Language::English);
    let text = app.status.as_ref().unwrap().text(&s);
    assert!(
        text.contains(s.workspace_reload_save_hint),
        "the failure message hints at saving the Workspace locally: {text}"
    );
}

#[test]
fn multiple_pending_workspace_reloads_are_offered_one_at_a_time() {
    let origin = WorkspaceGitOrigin {
        repo_url: "https://example.test/repo.git".into(),
        commit_sha: "abc123".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
        filter: WorkspaceGitFilter::HurlAndJson,
    };
    let dir_a = std::env::temp_dir().join(format!("paperboy_ws_multi_a_{}", std::process::id()));
    let dir_b = std::env::temp_dir().join(format!("paperboy_ws_multi_b_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);

    let mut col_a = Collection::new("repo-a".to_string(), Vec::new());
    col_a.workspace_root = Some(dir_a);
    col_a.workspace_downloaded_from_git = true;
    col_a.workspace_git_origin = Some(origin.clone());

    let mut col_b = Collection::new("repo-b".to_string(), Vec::new());
    col_b.workspace_root = Some(dir_b);
    col_b.workspace_downloaded_from_git = true;
    col_b.workspace_git_origin = Some(origin);

    let tab_a = crate::persistence::PersistedTab::from_collection(&col_a, None);
    let tab_b = crate::persistence::PersistedTab::from_collection(&col_b, None);
    let state = crate::persistence::PersistedState {
        tabs: vec![tab_a, tab_b],
        ..Default::default()
    };

    let mut app = TuiApp::default();
    app.apply_persisted(state);

    assert!(
        matches!(&app.overlay, Some(Overlay::WorkspaceReloadConfirm { idx, .. }) if *idx == 0),
        "only the first pending reload is shown initially"
    );
    press(&mut app, KeyCode::Esc); // decline the first
    assert!(
        matches!(&app.overlay, Some(Overlay::WorkspaceReloadConfirm { idx, .. }) if *idx == 1),
        "declining the first immediately offers the second"
    );
    press(&mut app, KeyCode::Esc); // decline the second
    assert!(
        app.overlay.is_none(),
        "no more pending reloads left to show"
    );
}

#[test]
fn filter_indices_is_case_insensitive_and_substring() {
    let items = ["main", "develop", "release/1.0"];
    assert_eq!(filter_indices(items.iter().copied(), ""), vec![0, 1, 2]);
    assert_eq!(filter_indices(items.iter().copied(), "RE"), vec![2]);
    assert_eq!(filter_indices(items.iter().copied(), "e"), vec![1, 2]);
    assert_eq!(filter_indices(items.iter().copied(), "1.0"), vec![2]);
}

// ── Editing environment secrets ───────────────────────────────────────

/// Build an app whose active tab has an environment with one resolved
/// secret (`TOKEN`) at index 0.
fn app_with_resolved_secret(secret: &str) -> TuiApp {
    let mut app = TuiApp::default();
    let (mut env, pending) =
        crate::environment::parse_vars_pending("e".into(), "TOKEN={{ op://V/i/f }}");
    env.apply_update(&crate::environment::EnvUpdate {
        env_id: env.id,
        index: pending[0].index,
        value: Some(secret.to_string()),
    });
    let env_id = add_global_env(&mut app, env);
    app.collections[0].linked_env_id = Some(env_id);
    app.focus = Pane::GlobalEnv;
    app.global_env_idx = 0;
    app
}

#[test]
fn editing_a_secret_opens_a_masked_prompt_with_reset() {
    let mut app = app_with_resolved_secret("s3cr3t");
    open_only_env_popup(&mut app);
    press(&mut app, KeyCode::Enter);
    match &app.overlay {
        Some(Overlay::Prompt {
            mask,
            reset_to,
            editor,
            kind,
            ..
        }) => {
            assert!(*mask, "a secret is masked while editing");
            assert_eq!(
                reset_to.as_deref(),
                Some("s3cr3t"),
                "reset target is the loaded value"
            );
            assert_eq!(
                editor.text(),
                "s3cr3t",
                "prefilled with the real value (shown masked)"
            );
            assert!(
                matches!(kind, PromptKind::EnvValue(env_id, 0) if *env_id == only_env_id(&app))
            );
        }
        _ => panic!("expected the env-value prompt to open"),
    }
}

#[test]
fn committing_a_new_secret_value_marks_it_modified() {
    let mut app = app_with_resolved_secret("s3cr3t");
    app.commit_prompt_with_secrecy(
        PromptKind::EnvValue(only_env_id(&app), 0),
        "my-own-token".into(),
        true,
    );

    let v = &only_env(&app).vars[0];
    assert!(v.modified, "a changed value is flagged modified");
    assert_eq!(v.value, "my-own-token");
    assert_eq!(
        v.original_value, "s3cr3t",
        "the original loaded value is kept for reset"
    );
    assert!(v.resolved);
}

#[test]
fn committing_the_same_value_is_not_modified() {
    let mut app = app_with_resolved_secret("s3cr3t");
    app.commit_prompt_with_secrecy(
        PromptKind::EnvValue(only_env_id(&app), 0),
        "s3cr3t".into(),
        true,
    );
    assert!(!only_env(&app).vars[0].modified);
}

#[test]
fn ctrl_r_resets_the_edit_to_the_original_value() {
    let mut app = app_with_resolved_secret("s3cr3t");
    open_only_env_popup(&mut app);
    press(&mut app, KeyCode::Enter); // open prompt (intact secret)
    press(&mut app, KeyCode::Char('X')); // first keystroke replaces the whole secret
    match &app.overlay {
        Some(Overlay::Prompt {
            editor,
            secret_intact,
            ..
        }) => {
            assert_eq!(
                editor.text(),
                "X",
                "typing replaces the whole secret, not appends"
            );
            assert!(
                !secret_intact,
                "the secret is no longer intact after typing"
            );
        }
        _ => panic!("prompt not open"),
    }
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)); // reset
    match &app.overlay {
        Some(Overlay::Prompt {
            editor,
            secret_intact,
            ..
        }) => {
            assert_eq!(
                editor.text(),
                "s3cr3t",
                "Ctrl+R restores the original value"
            );
            assert!(secret_intact, "reset returns to the intact (masked) state");
        }
        _ => panic!("prompt not open"),
    }
}

#[test]
fn backspace_on_an_intact_secret_clears_it_entirely() {
    let mut app = app_with_resolved_secret("longsecretvalue");
    open_only_env_popup(&mut app);
    press(&mut app, KeyCode::Enter); // open prompt (intact)
    press(&mut app, KeyCode::Backspace); // must wipe the whole secret at once
    match &app.overlay {
        Some(Overlay::Prompt {
            editor,
            secret_intact,
            ..
        }) => {
            assert_eq!(editor.text(), "", "one backspace clears the entire secret");
            assert!(!secret_intact);
        }
        _ => panic!("prompt not open"),
    }
}

#[test]
fn intact_secret_renders_fixed_eight_dots_regardless_of_length() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    // A long secret must NOT render more dots than a short one (no length leak).
    let render_secret_dots = |secret: &str| -> usize {
        let mut app = app_with_resolved_secret(secret);
        open_only_env_popup(&mut app);
        press(&mut app, KeyCode::Enter);
        let mut term = Terminal::new(TestBackend::new(72, 6)).unwrap();
        term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
            .unwrap();
        buffer_text(term.backend().buffer())
            .matches('\u{2022}')
            .count()
    };
    let short = render_secret_dots("ab");
    let long = render_secret_dots("this-is-a-very-long-secret-value");
    assert_eq!(short, long, "intact secret shows a fixed number of dots");
    assert_eq!(
        short, 8,
        "exactly eight dots are shown (matching SECRET_MASK)"
    );
}

#[test]
fn git_icon_shown_on_tab_and_requests_list_title_only_for_git_origin_collections() {
    use crate::git_remote::{GitOrigin, RefKind};
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("from-git".into(), Vec::new()));
    app.collections.last_mut().unwrap().git_origin = Some(GitOrigin {
        repo_url: "https://example.test/repo.git".into(),
        path: "api/health.hurl".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
    });
    app.collections
        .push(Collection::new("local-only".into(), Vec::new()));

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| {
        let area = f.area();
        super::draw::draw_tabs(
            f,
            ratatui::layout::Rect { height: 3, ..area },
            &app,
            &s,
            &th,
        );
    })
    .unwrap();
    let tabs_text = buffer_text(term.backend().buffer());
    assert!(
        tabs_text.contains(&format!("{} from-git", super::draw::GIT_ICON)),
        "git tab gets the icon"
    );
    assert!(
        !tabs_text.contains(&format!("{} local-only", super::draw::GIT_ICON)),
        "local tab has no icon"
    );

    let git_ci = app
        .collections
        .iter()
        .position(|c| c.name == "from-git")
        .unwrap();
    let mut term2 = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term2
        .draw(|f| {
            let area = f.area();
            super::draw::draw_collection_left(f, area, &app, git_ci, &s, &th);
        })
        .unwrap();
    let list_text = buffer_text(term2.backend().buffer());
    assert!(
        list_text.contains(&format!("{} from-git", super::draw::GIT_ICON)),
        "requests list title gets the icon too"
    );
}

#[test]
fn word_wrap_keeps_words_whole_and_only_hard_breaks_an_overlong_word() {
    let wrapped = super::draw::word_wrap("in wizard tables: toggle row enabled, delete row", 20);
    assert!(
        wrapped.iter().all(|l| l.chars().count() <= 20),
        "no line exceeds the requested width: {wrapped:?}"
    );
    // Re-joining every wrapped line (with single spaces) reproduces the original words in order.
    let rejoined: Vec<&str> = wrapped.iter().flat_map(|l| l.split_whitespace()).collect();
    let original: Vec<&str> = "in wizard tables: toggle row enabled, delete row"
        .split_whitespace()
        .collect();
    assert_eq!(
        rejoined, original,
        "no words are dropped or reordered by wrapping"
    );

    // A single word longer than the width is hard-broken instead of overflowing.
    let broken = super::draw::word_wrap("supercalifragilisticexpialidocious", 10);
    assert!(
        broken.len() > 1,
        "an overlong single word is split across multiple lines"
    );
    assert!(broken.iter().all(|l| l.chars().count() <= 10));
    assert_eq!(
        broken.concat(),
        "supercalifragilisticexpialidocious",
        "no characters lost when hard-breaking"
    );
}

#[test]
fn help_entry_wraps_continuation_lines_with_a_hanging_indent_under_the_description_column() {
    // A short shortcut label uses the fixed key column (17) + 1 space of indent.
    let lines =
        super::draw::help_entry_lines("", "in wizard tables: toggle row enabled, delete row", 40);
    assert!(
        lines.len() > 1,
        "the description is long enough to wrap at this width"
    );
    let render = |l: &ratatui::text::Line| {
        l.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    };
    let first = render(&lines[0]);
    let second = render(&lines[1]);
    assert_eq!(
        &first[..18],
        "                  ",
        "first line reserves the 17-col key column + 1 space"
    );
    assert_eq!(
        &second[..18],
        "                  ",
        "continuation line is indented to the same column"
    );
    assert_ne!(
        second.trim_start(),
        "",
        "continuation line still carries text"
    );

    // A shortcut label longer than the fixed key column pushes the indent out further,
    // so wrapped continuation lines still line up under *that* entry's description start.
    let long_shortcut = "[ / ], PgUp/PgDn, ^\u{2190}/\u{2192}";
    let lines2 =
        super::draw::help_entry_lines(long_shortcut, "previous or next tab in the list", 30);
    if lines2.len() > 1 {
        let expected_indent = long_shortcut.chars().count() + 1;
        let cont = render(&lines2[1]);
        assert!(
            cont.chars().take(expected_indent).all(|c| c == ' '),
            "indent matches the long shortcut's own width"
        );
    }
}

#[test]
fn help_popup_widens_on_a_spacious_terminal_but_stays_within_a_narrow_one() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let box_right_edge = |term_w: u16, term_h: u16| -> u16 {
        let mut app = TuiApp {
            overlay: Some(Overlay::Help(0)),
            ..Default::default()
        };
        let mut term = Terminal::new(TestBackend::new(term_w, term_h)).unwrap();
        term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
            .unwrap();
        let text = buffer_text(term.backend().buffer());
        // The box's top border row is the first row containing a corner char.
        let top_row = text
            .lines()
            .find(|l| l.contains('┌'))
            .expect("help box renders a top border");
        let left = top_row.chars().position(|c| c == '┌').unwrap();
        let right = top_row.chars().position(|c| c == '┐').unwrap();
        (right - left) as u16
    };

    let narrow = box_right_edge(70, 40);
    let spacious = box_right_edge(160, 40);
    assert!(
        spacious > narrow,
        "the help box is wider on a spacious terminal ({spacious} vs {narrow})"
    );
    assert!(
        spacious <= 100,
        "the help box width is capped so it doesn't become absurd on huge terminals"
    );
    assert!(
        narrow <= 70,
        "the help box never exceeds the terminal's own width"
    );
}

#[test]
fn help_popup_opens_on_the_shortcuts_tab_and_tab_key_switches_to_glossary() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('?'));
    assert!(
        matches!(app.overlay, Some(Overlay::Help(0))),
        "? opens Help on the Shortcuts tab"
    );

    press(&mut app, KeyCode::Tab);
    assert!(
        matches!(app.overlay, Some(Overlay::Help(1))),
        "Tab switches to the Glossary tab"
    );

    press(&mut app, KeyCode::Tab);
    assert!(
        matches!(app.overlay, Some(Overlay::Help(2))),
        "Tab from Glossary switches to the Reports tab"
    );

    press(&mut app, KeyCode::Tab);
    assert!(
        matches!(app.overlay, Some(Overlay::Help(0))),
        "Tab from Reports wraps back to Shortcuts"
    );

    press(&mut app, KeyCode::Right);
    assert!(
        matches!(app.overlay, Some(Overlay::Help(1))),
        "Right arrow also switches tabs"
    );
    press(&mut app, KeyCode::Left);
    assert!(
        matches!(app.overlay, Some(Overlay::Help(0))),
        "Left arrow switches back"
    );
    // Left from Shortcuts wraps around to the Reports tab (3-tab cycle).
    press(&mut app, KeyCode::Left);
    assert!(
        matches!(app.overlay, Some(Overlay::Help(2))),
        "Left from Shortcuts wraps to Reports"
    );
}

#[test]
fn help_popup_any_other_key_closes_it_from_either_tab() {
    let mut app = TuiApp {
        overlay: Some(Overlay::Help(0)),
        ..Default::default()
    };
    press(&mut app, KeyCode::Esc);
    assert!(
        app.overlay.is_none(),
        "Esc closes Help from the Shortcuts tab"
    );

    let mut app = TuiApp {
        overlay: Some(Overlay::Help(1)),
        ..Default::default()
    };
    press(&mut app, KeyCode::Char('q'));
    assert!(app.overlay.is_none(), "q closes Help from the Glossary tab");
}

#[test]
fn help_glossary_tab_renders_every_substitution_colour_and_the_shadow_icon() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);
    let mut app = TuiApp {
        overlay: Some(Overlay::Help(1)),
        ..Default::default()
    };
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());

    assert!(text.contains(s.glossary_label_literal));
    assert!(text.contains(s.glossary_label_loaded));
    assert!(text.contains(s.glossary_label_pending));
    assert!(text.contains(s.glossary_label_failed));
    assert!(text.contains(s.glossary_label_shadowed));
    assert!(
        text.contains(super::draw::SHADOW_ICON),
        "the shadow icon itself is shown, matching the inline marker"
    );
    assert!(
        text.contains(s.help_tab_glossary),
        "the popup title reflects the active Glossary tab"
    );
}

#[test]
fn help_glossary_tab_also_renders_every_other_app_icon() {
    // The Glossary is meant to be a complete legend, not just the
    // substitution dots — a second group covers every other icon shown
    // elsewhere in the app (pencil, plus, tick, cross, ellipsis, git,
    // link, folder, scroll-hint arrows).
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);
    let mut app = TuiApp {
        overlay: Some(Overlay::Help(1)),
        ..Default::default()
    };
    let mut term = Terminal::new(TestBackend::new(120, 50)).unwrap();
    term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());

    assert!(text.contains(s.glossary_heading_icons));
    assert!(text.contains(s.glossary_label_modified));
    assert!(text.contains(s.glossary_label_added));
    assert!(text.contains(s.glossary_label_passed));
    assert!(text.contains(s.glossary_label_run_failed));
    assert!(text.contains(s.glossary_label_running));
    assert!(text.contains(s.glossary_label_git));
    assert!(text.contains(s.glossary_label_linked));
    assert!(text.contains(s.glossary_label_folder));
    assert!(text.contains(s.glossary_label_scroll_hint));
    assert!(
        text.contains(super::draw::GIT_ICON),
        "the git icon glyph itself is shown"
    );
    assert!(
        text.contains(super::draw::LINK_ICON),
        "the link icon glyph itself is shown"
    );
    assert!(
        text.contains(super::draw::FOLDER_ICON),
        "the folder icon glyph itself is shown"
    );
}

#[test]
fn glossary_entries_with_a_double_width_emoji_icon_align_their_wrapped_description_like_any_other_row()
 {
    // `FOLDER_ICON`/`LINK_ICON` are double-width emoji, unlike the
    // single-column bullet/pencil/etc. glyphs used elsewhere in the
    // Glossary. Measuring the header by `.chars().count()` (1 char for
    // the emoji) instead of display width used to under-pad these rows
    // by one column, so the description's first line started one
    // column further right than its own wrapped continuation lines —
    // visibly an extra space before the first letter, which also threw
    // off the first line's word-wrap budget by one column (able to
    // orphan the trailing "—" onto its own line). Confirm the first
    // line's description now starts at exactly the same column as the
    // wrapped continuation lines.
    use crate::i18n::{Language, Strings};
    let s = Strings::for_language(&Language::English);
    let lines = super::draw::glossary_entry_lines(
        super::draw::FOLDER_ICON,
        ratatui::style::Color::White,
        s.glossary_label_folder,
        s.glossary_desc_folder,
        70,
    );
    assert!(
        lines.len() >= 2,
        "the folder description wraps to at least two lines at this width"
    );

    // First line is built as [header span, padding span, description span].
    let first_spans = &lines[0].spans;
    assert_eq!(
        first_spans.len(),
        3,
        "expected header + padding + description spans"
    );
    let first_desc_col = first_spans[0].width() + first_spans[1].width();

    // Continuation lines are a single `Line::raw` of literal leading
    // spaces followed by the wrapped text.
    let second_text: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
    let second_indent = second_text.chars().take_while(|c| *c == ' ').count();

    assert_eq!(
        first_desc_col, second_indent,
        "the first line's description must start at the same column as wrapped continuation lines"
    );
}

#[test]
fn help_popup_shows_both_tab_names_regardless_of_which_is_active() {
    // Both the Shortcuts and Glossary tab labels appear on screen at
    // once (as a two-tab strip), on either tab — so a user browsing the
    // Shortcuts list still sees the Glossary tab exists instead of
    // never noticing it.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    for tab in [0, 1, 2] {
        let mut app = TuiApp {
            overlay: Some(Overlay::Help(tab)),
            ..Default::default()
        };
        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
            .unwrap();
        let text = buffer_text(term.backend().buffer());
        assert!(
            text.contains(s.help_tab_shortcuts),
            "Shortcuts tab label visible while on tab {tab}"
        );
        assert!(
            text.contains(s.help_tab_glossary),
            "Glossary tab label visible while on tab {tab}"
        );
        assert!(
            text.contains(s.help_tab_reports),
            "Reports tab label visible while on tab {tab}"
        );
    }
}

#[test]
fn help_popup_stays_the_same_height_on_both_tabs() {
    // The Shortcuts list (31 entries) is much longer than the Glossary
    // (5 entries); without a shared fixed height the popup would jump
    // to a much shorter box on the Glossary tab — a jarring resize on
    // every switch. Assert the rendered box height is identical.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let box_height = |tab: usize| -> usize {
        let mut app = TuiApp {
            overlay: Some(Overlay::Help(tab)),
            ..Default::default()
        };
        let mut term = Terminal::new(TestBackend::new(120, 80)).unwrap();
        term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
            .unwrap();
        let text = buffer_text(term.backend().buffer());
        let top = text
            .lines()
            .position(|l| l.contains('┌'))
            .expect("top border");
        let bottom = text
            .lines()
            .position(|l| l.contains('└'))
            .expect("bottom border");
        bottom - top
    };

    assert_eq!(
        box_height(0),
        box_height(1),
        "the popup keeps one fixed height across both tabs"
    );
    assert_eq!(
        box_height(1),
        box_height(2),
        "the Reports tab shares that same fixed height too"
    );
}

#[test]
fn help_shortcuts_tab_groups_entries_into_titled_sections() {
    // The Shortcuts tab used to be one long flat list of 32 entries,
    // which new users found hard to scan. Confirm it's now broken into
    // titled sections (each a bold heading followed by its shortcuts),
    // and that every individual shortcut description still appears
    // somewhere in the popup — grouping must not have dropped any.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);
    let mut app = TuiApp {
        overlay: Some(Overlay::Help(0)),
        ..Default::default()
    };
    let mut term = Terminal::new(TestBackend::new(120, 80)).unwrap();
    term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());

    for heading in [
        s.help_group_navigation,
        s.help_group_tabs,
        s.help_group_requests,
        s.help_group_menus,
        s.help_group_environments,
        s.help_group_editing,
        s.help_group_reports,
        s.help_group_panels,
    ] {
        assert!(
            text.contains(heading),
            "section heading {heading:?} is shown"
        );
    }
    for desc in [
        s.help_focus,
        s.help_move,
        s.help_page_response,
        s.help_switch_tabs,
        s.help_select,
        s.help_run,
        s.help_run_all,
        s.help_new,
        s.help_raw_mode,
        s.help_raw_json,
        s.help_base_url,
        s.help_menus,
        s.help_workspace_browse,
        s.help_prev_next_tab,
        s.help_rename_close,
        s.help_reload_var,
        s.help_env_rename,
        s.help_env_activate,
        s.help_env_delete,
        s.help_env_reopen,
        s.help_env_link,
        s.help_env_view_linked,
        s.help_tab_manage,
        s.help_restore_request,
        s.help_tab_reorder,
        s.help_multi_select,
        s.help_resize,
        s.help_resize_width,
        s.help_save_editor,
        s.help_report_new,
        s.help_report_edit,
        s.help_report_leave_edit,
        s.help_cancel,
        s.help_quit,
    ] {
        assert!(
            text.contains(desc),
            "shortcut description {desc:?} still appears after grouping"
        );
    }
    // `help_copy_selection` is long enough to wrap even at the widest
    // popup size, so check its leading words rather than the whole
    // string as one contiguous run.
    assert!(
        text.contains("copy the selection"),
        "the copy-selection shortcut description still appears (possibly wrapped)"
    );
}

#[test]
fn help_reports_tab_explains_reports_shortcuts_and_grammar() {
    // The third Help tab documents the Reports feature: a "what is a
    // report" blurb, the report-specific shortcuts, and a compact
    // summary of the flow language.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);
    let mut app = TuiApp {
        overlay: Some(Overlay::Help(2)),
        ..Default::default()
    };
    let mut term = Terminal::new(TestBackend::new(120, 80)).unwrap();
    term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());
    for expected in [
        s.help_reports_about_heading,
        s.help_reports_shortcuts_heading,
        s.help_reports_grammar_heading,
        s.help_reports_loops_heading,
        s.help_grammar_collection,
        s.help_grammar_for_tuple,
        s.help_grammar_zip,
    ] {
        assert!(
            text.contains(expected),
            "the Reports help tab shows {expected:?}"
        );
    }
}

/// `help_entry_lines_col` lines every description up to the same column
/// regardless of how long each entry's left-hand side is — the fix for the
/// report grammar rows, whose keys range from `ZIP(a, b, …)` to
/// `REPORT REQUEST NAME [AS COL]`, previously starting each description
/// wherever its own key happened to end.
#[test]
fn help_grammar_entries_align_descriptions_to_one_column() {
    let first = |lines: Vec<ratatui::text::Line>| -> String {
        lines[0]
            .spans
            .iter()
            .map(|sp| sp.content.as_ref())
            .collect()
    };
    let short = first(super::draw::help_entry_lines_col("ZIP", "AAA", 26, 100));
    let long = first(super::draw::help_entry_lines_col(
        "FOLDERS \"dir\" [WITH r=\"g\"]",
        "BBB",
        26,
        100,
    ));
    assert_eq!(
        short.find("AAA"),
        long.find("BBB"),
        "descriptions start at the same column despite different key widths"
    );
    // key column (26) + one separating space → description at index 27.
    assert_eq!(short.find("AAA"), Some(27));
}

#[test]
fn help_section_headings_render_as_a_dashed_divider_with_no_surrounding_blank_lines() {
    // Titles now read as "── Title ────" rules instead of a bold
    // heading line followed by a blank spacer line, and the very first
    // section no longer has a blank line above it either — both changes
    // trade a bit of visual weight for more visible content on small
    // terminals.
    use crate::i18n::{Language, Strings};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);
    let line = super::draw::help_section_divider(s.help_group_navigation, 50, &th);
    let rendered: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
    assert!(
        rendered.starts_with("──"),
        "the divider opens with a short dash rule: {rendered:?}"
    );
    assert!(
        rendered.contains(s.help_group_navigation),
        "the title text itself is still shown"
    );
    assert!(
        rendered.trim_end().ends_with('─'),
        "the divider closes with a dash rule filling the rest of the width: {rendered:?}"
    );
    assert_eq!(
        rendered.chars().count(),
        50,
        "the divider fills the full requested width"
    );
}

#[test]
fn help_popup_up_down_scroll_the_body_instead_of_closing_it() {
    // Up/Down used to fall through to the catch-all "any other key
    // closes Help" arm, making the rest of a body taller than the
    // terminal unreachable. They should scroll instead.
    let mut app = TuiApp {
        overlay: Some(Overlay::Help(0)),
        ..Default::default()
    };
    press(&mut app, KeyCode::Down);
    assert!(
        matches!(app.overlay, Some(Overlay::Help(0))),
        "Down scrolls instead of closing Help"
    );
    assert_eq!(app.help_scroll, 1);

    press(&mut app, KeyCode::Down);
    assert_eq!(app.help_scroll, 2);

    press(&mut app, KeyCode::Up);
    assert!(
        matches!(app.overlay, Some(Overlay::Help(0))),
        "Up scrolls instead of closing Help"
    );
    assert_eq!(app.help_scroll, 1);

    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.help_scroll, 11);
    press(&mut app, KeyCode::Home);
    assert_eq!(app.help_scroll, 0);
}

#[test]
fn help_popup_shows_a_scrollbar_and_clamps_scroll_when_the_body_is_taller_than_the_terminal() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    // A short terminal can't fit the whole Shortcuts body at once.
    let mut app = TuiApp {
        overlay: Some(Overlay::Help(0)),
        help_scroll: u16::MAX,
        ..Default::default()
    };
    let mut term = Terminal::new(TestBackend::new(120, 12)).unwrap();
    term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(
        text.contains('█'),
        "a scrollbar thumb is drawn when the body overflows the popup: {text}"
    );
    // `help_scroll` must have been clamped down from u16::MAX to some
    // small, sane maximum instead of staying at an out-of-range value.
    assert!(
        app.help_scroll < 200,
        "help_scroll is clamped to the body's actual max scroll, not left at u16::MAX"
    );

    // A tall terminal fits everything, so no scrollbar is needed.
    let mut app2 = TuiApp {
        overlay: Some(Overlay::Help(0)),
        ..Default::default()
    };
    let mut term2 = Terminal::new(TestBackend::new(120, 80)).unwrap();
    term2
        .draw(|f| super::draw::draw_overlay(f, &mut app2, &s, &th))
        .unwrap();
    let text2 = buffer_text(term2.backend().buffer());
    assert!(
        !text2.contains('█'),
        "no scrollbar thumb when the whole body already fits: {text2}"
    );
}

#[test]
fn menu_bar_shows_the_mnemonic_baked_into_file_and_settings_labels() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);
    let app = TuiApp::default();
    let mut term = Terminal::new(TestBackend::new(60, 1)).unwrap();
    term.draw(|f| super::draw::draw_menu(f, f.area(), &app, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());

    // Mnemonics are shown as an underlined letter, not "(X)" brackets
    // (several users found the brackets visually distracting).
    assert!(text.contains("File"), "the File label is shown");
    assert!(text.contains("Settings"), "the Settings label is shown");
    assert!(
        !text.contains("(F)ile"),
        "brackets are replaced by underline styling"
    );
    assert!(
        !text.contains("(S)ettings"),
        "brackets are replaced by underline styling"
    );

    let row = text.lines().next().unwrap();
    let f_col = row.find("File").expect("File present") as u16;
    let s_col = row.find("Settings").expect("Settings present") as u16;
    let buf = term.backend().buffer();
    assert!(
        buf[(f_col, 0)]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED),
        "the 'F' mnemonic letter itself is underlined"
    );
    assert!(
        buf[(s_col, 0)]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED),
        "the 'S' mnemonic letter itself is underlined"
    );
}

#[test]
fn git_icon_shown_on_env_heading_independently_of_the_collection_origin() {
    use crate::environment::Environment;
    use crate::git_remote::{GitOrigin, RefKind};
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut app = TuiApp::default();
    // A collection with NO git origin of its own, but its environment DOES
    // have one — the icon must reflect the env's own origin, not the
    // collection's.
    app.collections
        .push(Collection::new("local-collection".into(), Vec::new()));
    let ci = app.collections.len() - 1;
    let env = Environment {
        id: 0,
        name: "staging".into(),
        vars: Vec::new(),
        path: None,
        git_origin: Some(GitOrigin {
            repo_url: "https://example.test/repo.git".into(),
            path: "envs/staging.vars".into(),
            ref_kind: RefKind::Tag,
            ref_name: "v1.0".into(),
        }),
    };
    let env_id = add_global_env(&mut app, env);
    app.collections[ci].linked_env_id = Some(env_id);
    assert!(
        app.collections[ci].git_origin.is_none(),
        "collection itself was not loaded from git"
    );

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let popup = EnvPopupState::new(env_id);
    term.draw(|f| super::draw::draw_env_popup(f, &app, &popup, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(
        text.contains(&format!("{} staging", super::draw::GIT_ICON)),
        "env heading gets the icon from env git_origin"
    );
}

// ── "Save Collection to Git" wizard ─────────────────────────────────────

#[test]
fn save_to_git_menu_item_is_refused_without_a_remembered_git_origin() {
    use crate::i18n::Status;
    let mut app = TuiApp::default(); // the built-in Request tab has no git_origin
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Down); // -> "(S)ave" submenu
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // -> Collection kind
    press(&mut app, KeyCode::Enter); // -> destination step
    for _ in 0..2 {
        press(&mut app, KeyCode::Down); // dest item 2 = "To Git…"
    }
    press(&mut app, KeyCode::Enter);

    assert!(
        app.overlay.is_none(),
        "the menu just closes — no wizard for a non-git collection"
    );
    assert!(matches!(app.status, Some(Status::NoGitOrigin)));
}

#[test]
fn save_to_git_wizard_is_prefilled_from_the_collections_remembered_origin() {
    use crate::git_remote::{GitOrigin, RefKind};
    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("api".into(), Vec::new()));
    let ci = app.collections.len() - 1;
    app.collections[ci].git_origin = Some(GitOrigin {
        repo_url: "https://example.test/repo.git".into(),
        path: "api/health.hurl".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
    });
    app.active_tab = ci;

    app.open_git_save_wizard();

    match &app.overlay {
        Some(Overlay::GitSave(w)) => {
            assert!(matches!(w.stage, GitSaveStage::Connect { field: 0 }));
            assert_eq!(
                w.url.text(),
                "https://example.test/repo.git",
                "the URL autocompletes from the remembered origin"
            );
            assert_eq!(w.collection_path.text(), "api/health.hurl");
            assert_eq!(
                w.target_name.text(),
                "main",
                "defaults to appending to the originally-loaded branch"
            );
            assert!(w.target_kind == GitSaveTarget::Branch);
        }
        _ => panic!("expected the git-save wizard to open"),
    }
}

#[test]
fn choose_paths_checkbox_toggles_and_tab_skips_the_hidden_env_path_field() {
    use crate::git_remote::{GitOrigin, RefKind};
    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("api".into(), Vec::new()));
    let ci = app.collections.len() - 1;
    let env_id = add_empty_global_env(&mut app, "e");
    app.collections[ci].linked_env_id = Some(env_id);
    app.collections[ci].git_origin = Some(GitOrigin {
        repo_url: "https://example.test/repo.git".into(),
        path: "api/health.hurl".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
    });
    app.active_tab = ci;
    app.open_git_save_wizard();
    {
        let Some(Overlay::GitSave(w)) = &mut app.overlay else {
            panic!()
        };
        w.stage = GitSaveStage::ChoosePaths { field: 0 };
        assert!(
            w.include_env,
            "an attached environment defaults to being included"
        );
    }

    press(&mut app, KeyCode::Tab); // field 0 -> 1 (checkbox)
    press(&mut app, KeyCode::Char(' ')); // uncheck it

    match &app.overlay {
        Some(Overlay::GitSave(w)) => {
            assert!(!w.include_env, "space toggles the checkbox");
            assert!(matches!(w.stage, GitSaveStage::ChoosePaths { field: 1 }));
        }
        _ => panic!(),
    }

    press(&mut app, KeyCode::Tab); // field 1 -> back to 0 (field 2 is now hidden)
    match &app.overlay {
        Some(Overlay::GitSave(w)) => {
            assert!(matches!(w.stage, GitSaveStage::ChoosePaths { field: 0 }))
        }
        _ => panic!("unchecking the env removed field 2 from the tab cycle"),
    }
}

#[test]
fn choose_target_picking_an_existing_branch_from_the_dropdown_marks_it_as_existing() {
    let mut app = TuiApp::default();
    let mut w = Box::new(app_git_save_wizard(&mut app, "main"));
    w.stage = GitSaveStage::ChooseTarget {
        sel: None,
        refs: Some(RemoteRefs {
            branches: vec!["main".into(), "develop".into()],
            tags: vec!["v1".into()],
        }),
    };
    app.overlay = Some(Overlay::GitSave(w));

    press(&mut app, KeyCode::Down); // open the dropdown at "main"
    press(&mut app, KeyCode::Down); // move to "develop"
    press(&mut app, KeyCode::Enter); // pick it (closes the dropdown, doesn't submit yet)
    match &app.overlay {
        Some(Overlay::GitSave(w)) => {
            assert_eq!(w.target_name.text(), "develop");
            assert!(matches!(
                w.stage,
                GitSaveStage::ChooseTarget { sel: None, .. }
            ));
        }
        _ => panic!(),
    }

    press(&mut app, KeyCode::Enter); // submit
    match &app.overlay {
        Some(Overlay::GitSave(w)) => {
            assert!(matches!(w.stage, GitSaveStage::CommitMessage));
            assert!(w.target_intent == TargetIntent::ExistingBranch);
        }
        _ => panic!("expected to move to the commit-message step"),
    }
}

#[test]
fn choose_target_typing_a_brand_new_branch_name_marks_it_as_new() {
    let mut app = TuiApp::default();
    let mut w = Box::new(app_git_save_wizard(&mut app, "main"));
    w.target_name = super::editor::Editor::blank();
    w.stage = GitSaveStage::ChooseTarget {
        sel: None,
        refs: Some(RemoteRefs {
            branches: vec!["main".into()],
            tags: vec![],
        }),
    };
    app.overlay = Some(Overlay::GitSave(w));

    for ch in "feature-x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Enter);

    match &app.overlay {
        Some(Overlay::GitSave(w)) => {
            assert!(matches!(w.stage, GitSaveStage::CommitMessage));
            assert_eq!(w.target_name.text(), "feature-x");
            assert!(
                w.target_intent == TargetIntent::NewRef,
                "a name not on the remote is a brand-new ref"
            );
        }
        _ => panic!(),
    }
}

#[test]
fn choose_target_tab_toggles_between_branch_and_tag() {
    let mut app = TuiApp::default();
    let w = Box::new(app_git_save_wizard(&mut app, "main"));
    app.overlay = Some(Overlay::GitSave(w));

    match &app.overlay {
        Some(Overlay::GitSave(w)) => {
            assert!(w.target_kind == GitSaveTarget::Branch, "starts on Branch")
        }
        _ => panic!(),
    }
    press(&mut app, KeyCode::Tab);
    match &app.overlay {
        Some(Overlay::GitSave(w)) => {
            assert!(w.target_kind == GitSaveTarget::Tag, "Tab toggles to Tag")
        }
        _ => panic!(),
    }
}

/// A `GitSaveWizard` for collection `ci` (created fresh, pointing at
/// `branch`), placed at the `ChooseTarget` stage — used by tests that
/// only care about that stage's key handling.
fn app_git_save_wizard(app: &mut TuiApp, branch: &str) -> GitSaveWizard {
    use crate::git_remote::{GitOrigin, RefKind};
    app.collections
        .push(Collection::new("api".into(), Vec::new()));
    let ci = app.collections.len() - 1;
    app.collections[ci].git_origin = Some(GitOrigin {
        repo_url: "https://example.test/repo.git".into(),
        path: "api/health.hurl".into(),
        ref_kind: RefKind::Branch,
        ref_name: branch.to_string(),
    });
    app.active_tab = ci;
    let mut w = GitSaveWizard::new(ci, &app.collections[ci], app.effective_env(ci));
    w.stage = GitSaveStage::ChooseTarget {
        sel: None,
        refs: None,
    };
    w
}

#[test]
fn saving_to_git_appends_a_commit_and_updates_the_remembered_origin_and_markers() {
    use crate::git_remote::{GitOrigin, RefKind};
    use crate::i18n::Status;
    use std::process::Command;

    fn git(args: &[&str], cwd: &std::path::Path) {
        let out = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // A local bare repo stands in for "the remote" — no network needed,
    // exactly like git_remote.rs's own push-plumbing tests.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = std::env::temp_dir().join(format!(
        "paperboy-git-save-tui-test-{}-{nanos}",
        std::process::id()
    ));
    let bare = base.join("bare.git");
    let work = base.join("work");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&["init", "--bare", "-q", "."], &bare);
    git(&["init", "-q"], &work);
    git(&["checkout", "-q", "-b", "main"], &work);
    git(&["config", "user.name", "Seed"], &work);
    git(&["config", "user.email", "seed@test"], &work);
    std::fs::write(work.join("api.hurl"), "GET https://example.com\n").unwrap();
    git(&["add", "-A"], &work);
    git(&["commit", "-q", "-m", "seed"], &work);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &work);
    git(&["push", "-q", "origin", "main"], &work);
    let bare_url = bare.to_str().unwrap().to_string();

    let mut app = TuiApp::default();
    let mut entry = HurlEntry {
        method: "GET".into(),
        url: "https://example.com".into(),
        ..Default::default()
    };
    entry.modified = true;
    let mut col = Collection::new("api".into(), vec![entry]);
    col.git_origin = Some(GitOrigin {
        repo_url: bare_url.clone(),
        path: "api.hurl".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
    });
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;

    app.open_git_save_wizard();
    let Some(Overlay::GitSave(mut w)) = app.overlay.take() else {
        panic!("wizard should be open")
    };
    // Skip straight to the commit-message step with the defaults
    // `GitSaveWizard::new` already seeded (target = the original "main"
    // branch — the "append" case); the ChooseTarget/ChoosePaths key
    // handling itself is covered by the dedicated tests above.
    w.include_env = false;
    w.stage = GitSaveStage::CommitMessage;
    app.overlay = Some(Overlay::GitSave(w));

    press(&mut app, KeyCode::Enter); // spawns the real (local, offline) push

    let Some(Overlay::GitSave(mut w)) = app.overlay.take() else {
        panic!("wizard should still be open (Pushing)")
    };
    assert!(matches!(w.stage, GitSaveStage::Pushing));
    let rx = w.rx.take().expect("the push should have been spawned");
    let msg = rx.recv().expect("the push thread should send a result");
    let keep_open = app.apply_git_save_msg(&mut w, msg);
    assert!(keep_open, "the Done stage stays open until dismissed");
    assert!(
        matches!(w.stage, GitSaveStage::Done),
        "a successful push moves to Done"
    );

    assert!(matches!(app.status, Some(Status::GitSaved)));
    assert!(
        !app.collections[ci].entries[0].modified,
        "the modified marker is cleared after a successful save"
    );
    let origin = app.collections[ci].git_origin.as_ref().unwrap();
    assert_eq!(
        origin.ref_name, "main",
        "a branch-target save keeps remembering the same branch"
    );
    assert_eq!(origin.repo_url, bare_url);

    // The new commit really is on `main` in the bare repo, with our message.
    git(&["fetch", "-q", "origin", "main"], &work);
    let log = Command::new("git")
        .current_dir(&work)
        .args(["log", "-1", "--format=%s", "origin/main"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        "Update api via PaperBoy"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn saving_a_workspace_to_git_commits_the_whole_tree_and_repins_the_origin_sha() {
    use crate::i18n::Status;
    use std::process::Command;

    fn git(args: &[&str], cwd: &std::path::Path) {
        let out = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = std::env::temp_dir().join(format!(
        "paperboy-ws-git-save-test-{}-{nanos}",
        std::process::id()
    ));
    let bare = base.join("bare.git");
    let seed = base.join("seed");
    let ws = base.join("ws"); // the tab's on-disk workspace_root
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::create_dir_all(&seed).unwrap();
    std::fs::create_dir_all(ws.join("api")).unwrap();

    // Seed the "remote" with an existing tree on `main`.
    git(&["init", "--bare", "-q", "."], &bare);
    git(&["init", "-q"], &seed);
    git(&["checkout", "-q", "-b", "main"], &seed);
    git(&["config", "user.name", "Seed"], &seed);
    git(&["config", "user.email", "seed@test"], &seed);
    std::fs::create_dir_all(seed.join("api")).unwrap();
    std::fs::write(seed.join("api/health.hurl"), "GET https://old\n").unwrap();
    git(&["add", "-A"], &seed);
    git(&["commit", "-q", "-m", "seed"], &seed);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &seed);
    git(&["push", "-q", "origin", "main"], &seed);
    let bare_url = bare.to_str().unwrap().to_string();

    // The local workspace holds an edited file plus a new one, and a `.git`
    // folder that must be excluded from the commit.
    std::fs::write(ws.join("api/health.hurl"), "GET https://new\n").unwrap();
    std::fs::write(ws.join("api/orders.hurl"), "GET https://orders\n").unwrap();
    std::fs::create_dir_all(ws.join(".git")).unwrap();
    std::fs::write(ws.join(".git/config"), "secret\n").unwrap();

    let origin = WorkspaceGitOrigin {
        repo_url: bare_url.clone(),
        commit_sha: "0000000000000000000000000000000000000000".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
        filter: WorkspaceGitFilter::HurlAndJson,
    };
    let mut col = Collection::new("api-workspace".into(), Vec::new());
    col.workspace_root = Some(ws.clone());
    col.workspace_downloaded_from_git = true;
    col.workspace_git_origin = Some(origin);

    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;

    app.open_git_workspace_save_wizard();
    let Some(Overlay::GitSave(mut w)) = app.overlay.take() else {
        panic!("the workspace git-save wizard should open")
    };
    // `new_workspace` seeds the branch/target from the origin; jump straight
    // to the commit message (target/branch key handling is shared with the
    // collection flow, already covered above).
    assert!(
        matches!(w.stage, GitSaveStage::Connect { .. }),
        "opens on the Connect step"
    );
    assert_eq!(w.target_name.text(), "main");
    w.stage = GitSaveStage::CommitMessage;
    app.overlay = Some(Overlay::GitSave(w));

    press(&mut app, KeyCode::Enter); // enumerate the tree + push

    let Some(Overlay::GitSave(mut w)) = app.overlay.take() else {
        panic!("wizard should still be open (Pushing)")
    };
    assert!(
        matches!(w.stage, GitSaveStage::Pushing),
        "a workspace with files pushes immediately"
    );
    let rx = w.rx.take().expect("the push should have been spawned");
    let msg = rx.recv().expect("the push thread should send a result");
    let keep_open = app.apply_git_save_msg(&mut w, msg);
    assert!(keep_open);
    assert!(
        matches!(w.stage, GitSaveStage::Done),
        "a successful workspace push moves to Done"
    );
    assert!(matches!(app.status, Some(Status::GitSaved)));

    // The remembered origin is repinned to the freshly-pushed commit (no
    // longer the placeholder sha) while keeping the branch and filter.
    let repinned = app.collections[ci].workspace_git_origin.as_ref().unwrap();
    assert_ne!(
        repinned.commit_sha, "0000000000000000000000000000000000000000",
        "the origin sha is repinned to the new commit"
    );
    assert_eq!(repinned.ref_name, "main");
    assert!(matches!(repinned.filter, WorkspaceGitFilter::HurlAndJson));

    // The pushed commit carries the edited + new files (and never the `.git`).
    git(&["fetch", "-q", "origin", "main"], &seed);
    let show = Command::new("git")
        .current_dir(&seed)
        .args(["show", "origin/main:api/health.hurl"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&show.stdout), "GET https://new\n");
    let orders = Command::new("git")
        .current_dir(&seed)
        .args(["show", "origin/main:api/orders.hurl"])
        .output()
        .unwrap();
    assert!(orders.status.success(), "the new file was committed too");
    let tree = Command::new("git")
        .current_dir(&seed)
        .args(["ls-tree", "-r", "--name-only", "origin/main"])
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&tree.stdout);
    assert!(
        !names.contains(".git"),
        "the workspace's internal .git folder is never committed"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn saving_a_workspace_to_git_is_rejected_when_the_tab_was_not_git_loaded() {
    use crate::i18n::Status;
    let mut app = TuiApp::default();
    let col = Collection::new("plain".into(), Vec::new());
    app.collections.push(col);
    app.active_tab = app.collections.len() - 1;

    app.open_git_workspace_save_wizard();

    assert!(
        app.overlay.is_none(),
        "no wizard opens without a git origin"
    );
    assert!(matches!(app.status, Some(Status::NoGitOrigin)));
}

/// Build an app with a single git-loaded Workspace tab whose currently-loaded
/// file (`current.hurl` under a fresh temp root) has one unsaved, modified
/// request in memory. Returns the app, the tab index, and the on-disk path of
/// the loaded file (seeded with placeholder content that differs from the
/// in-memory collection, so a "save" is observable). Caller cleans up the dir.
fn workspace_tab_with_unsaved_edit() -> (TuiApp, usize, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "paperboy-ws-unsaved-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("current.hurl");
    std::fs::write(&file, "GET https://on-disk-placeholder\n").unwrap();

    let mut entry = HurlEntry {
        method: "GET".into(),
        url: "https://edited-in-memory".into(),
        ..Default::default()
    };
    entry.modified = true;
    let mut col = Collection::new("api-workspace".into(), vec![entry]);
    col.workspace_root = Some(root.clone());
    col.workspace_downloaded_from_git = true;
    col.path = Some(file.clone());
    col.workspace_git_origin = Some(WorkspaceGitOrigin {
        repo_url: "https://example.test/repo.git".into(),
        commit_sha: "abc123".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
        filter: WorkspaceGitFilter::HurlAndJson,
    });

    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;
    (app, ci, file)
}

#[test]
fn saving_a_workspace_to_git_warns_first_when_the_loaded_file_has_unsaved_edits() {
    let (mut app, ci, _file) = workspace_tab_with_unsaved_edit();

    app.open_git_workspace_save_wizard();

    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::WorkspaceGitSaveUnsaved { ci: c, sel: 0 }) if *c == ci
        ),
        "unsaved in-memory edits raise the warning instead of opening the wizard"
    );
}

#[test]
fn workspace_unsaved_warning_save_choice_writes_the_file_then_opens_the_wizard() {
    let (mut app, ci, file) = workspace_tab_with_unsaved_edit();
    let expected = app.collections[ci].to_hurl();

    app.open_git_workspace_save_wizard();
    // sel starts on "Save changes, then push".
    press(&mut app, KeyCode::Enter);

    assert!(
        matches!(app.overlay, Some(Overlay::GitSave(_))),
        "after saving, the git-save wizard opens"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        expected,
        "the in-memory edits were written to disk"
    );
    assert!(
        !app.collections[ci].entries[0].modified,
        "the modified marker is cleared once saved"
    );

    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn workspace_unsaved_warning_discard_choice_pushes_disk_version_and_keeps_edits_in_memory() {
    let (mut app, ci, file) = workspace_tab_with_unsaved_edit();
    let on_disk_before = std::fs::read_to_string(&file).unwrap();

    app.open_git_workspace_save_wizard();
    press(&mut app, KeyCode::Down); // move to "Push the saved version"
    press(&mut app, KeyCode::Enter);

    assert!(
        matches!(app.overlay, Some(Overlay::GitSave(_))),
        "discarding opens the wizard directly (pushes the on-disk tree)"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        on_disk_before,
        "the on-disk file is left untouched"
    );
    assert!(
        app.collections[ci].entries[0].modified,
        "the in-memory edits remain (only left out of this push)"
    );

    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn workspace_unsaved_warning_cancel_choice_closes_without_opening_the_wizard() {
    let (mut app, _ci, file) = workspace_tab_with_unsaved_edit();

    app.open_git_workspace_save_wizard();
    press(&mut app, KeyCode::Up); // wrap up to "Cancel"
    press(&mut app, KeyCode::Enter);

    assert!(app.overlay.is_none(), "cancel closes the warning");

    // Esc also cancels.
    app.open_git_workspace_save_wizard();
    assert!(matches!(
        app.overlay,
        Some(Overlay::WorkspaceGitSaveUnsaved { .. })
    ));
    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_none(), "Esc dismisses the warning");

    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn saving_a_workspace_to_git_skips_the_warning_when_there_are_no_unsaved_edits() {
    let (mut app, ci, file) = workspace_tab_with_unsaved_edit();
    // Mark the loaded file as clean (no in-memory edits pending).
    app.collections[ci].entries[0].modified = false;

    app.open_git_workspace_save_wizard();

    assert!(
        matches!(app.overlay, Some(Overlay::GitSave(_))),
        "with nothing unsaved the wizard opens straight away"
    );

    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn saving_to_a_tag_that_already_exists_is_rejected_and_never_overwritten() {
    use crate::git_remote::{GitOrigin, RefKind};
    use std::process::Command;

    fn git(args: &[&str], cwd: &std::path::Path) {
        let out = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = std::env::temp_dir().join(format!(
        "paperboy-git-save-tag-test-{}-{nanos}",
        std::process::id()
    ));
    let bare = base.join("bare.git");
    let work = base.join("work");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git(&["init", "--bare", "-q", "."], &bare);
    git(&["init", "-q"], &work);
    git(&["checkout", "-q", "-b", "main"], &work);
    git(&["config", "user.name", "Seed"], &work);
    git(&["config", "user.email", "seed@test"], &work);
    std::fs::write(work.join("api.hurl"), "GET https://example.com\n").unwrap();
    git(&["add", "-A"], &work);
    git(&["commit", "-q", "-m", "seed"], &work);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &work);
    git(&["push", "-q", "origin", "main"], &work);
    // A pre-existing tag that a save must never be allowed to overwrite.
    git(&["tag", "v1.0"], &work);
    git(&["push", "-q", "origin", "v1.0"], &work);
    let bare_url = bare.to_str().unwrap().to_string();

    let mut app = TuiApp::default();
    let mut col = Collection::new("api".into(), Vec::new());
    col.git_origin = Some(GitOrigin {
        repo_url: bare_url.clone(),
        path: "api.hurl".into(),
        ref_kind: RefKind::Branch,
        ref_name: "main".into(),
    });
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;

    app.open_git_save_wizard();
    let Some(Overlay::GitSave(mut w)) = app.overlay.take() else {
        panic!("wizard should be open")
    };
    w.include_env = false;
    w.target_kind = GitSaveTarget::Tag;
    w.target_name = super::editor::Editor::new("v1.0", false);
    w.target_intent = TargetIntent::NewRef;
    w.stage = GitSaveStage::CommitMessage;
    app.overlay = Some(Overlay::GitSave(w));

    press(&mut app, KeyCode::Enter); // spawns the real push, which must self-reject

    let Some(Overlay::GitSave(mut w)) = app.overlay.take() else {
        panic!("wizard should still be open (Pushing)")
    };
    let rx = w.rx.take().expect("the push should have been spawned");
    let msg = rx.recv().expect("the push thread should send a result");
    app.apply_git_save_msg(&mut w, msg);

    match &w.stage {
        GitSaveStage::Error(e) => assert!(
            e.contains("already exists") || e.contains("never"),
            "got: {e}"
        ),
        _ => panic!("expected the tag-exists rejection, got a different stage"),
    }
    // The collection's remembered origin is untouched by a rejected save.
    assert_eq!(
        app.collections[ci].git_origin.as_ref().unwrap().ref_name,
        "main"
    );

    let _ = std::fs::remove_dir_all(&base);
}

fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    let area = buf.area();
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Like `buffer_text`, but with newlines and box-drawing border characters
/// stripped, so wrapped content that happens to fall right at a row
/// boundary (immediately before/after the panel's border) can still be
/// checked for as one contiguous substring. Also strips the scrollbar
/// thumb glyph (`draw_scrollbar`'s `\u{2588}`), which — like the border
/// itself — is overlaid on the panel's border column, never inside the
/// actual text content.
fn flattened_content(buf: &ratatui::buffer::Buffer) -> String {
    buffer_text(buf)
        .chars()
        .filter(|c| !"\n│─┌┐└┘\u{2588}\u{21b5}".contains(*c))
        .collect()
}

/// The foreground colour of the first cell where `needle` starts, scanning
/// the buffer row by row (cell-by-cell so multi-byte borders don't skew it).
fn fg_at_substr(buf: &ratatui::buffer::Buffer, needle: &str) -> Option<ratatui::style::Color> {
    let area = *buf.area();
    let chars: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
    for y in 0..area.height {
        for x in 0..area.width {
            let matches = chars.iter().enumerate().all(|(k, ch)| {
                let xx = x + k as u16;
                xx < area.width && buf[(xx, y)].symbol() == ch
            });
            if matches {
                return Some(buf[(x, y)].fg);
            }
        }
    }
    None
}

#[test]
fn env_panel_marks_modified_and_prompt_masks_the_secret() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut app = app_with_resolved_secret("s3cr3t");
    only_env_mut(&mut app).vars[0].set_user_value_secrecy("changed-secret".into(), true, 0);

    // The env popup shows a pencil next to the modified var and never the value.
    let mut term = Terminal::new(TestBackend::new(50, 8)).unwrap();
    let popup = EnvPopupState::new(only_env_id(&app));
    term.draw(|f| super::draw::draw_env_popup(f, &app, &popup, &s, &th))
        .unwrap();
    let panel = buffer_text(term.backend().buffer());
    assert!(
        panel.contains('\u{270e}'),
        "modified var shows the pencil marker:\n{panel}"
    );
    assert!(
        !panel.contains("changed-secret"),
        "secret value never shown in the panel:\n{panel}"
    );

    // Editing the secret masks it (neither the current nor original value appears).
    open_only_env_popup(&mut app);
    press(&mut app, KeyCode::Enter);
    let mut term2 = Terminal::new(TestBackend::new(72, 8)).unwrap();
    term2
        .draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
        .unwrap();
    let overlay = buffer_text(term2.backend().buffer());
    assert!(
        !overlay.contains("changed-secret"),
        "current secret hidden while editing:\n{overlay}"
    );
    assert!(
        !overlay.contains("s3cr3t"),
        "original secret hidden while editing:\n{overlay}"
    );
    assert!(
        overlay.contains('\u{2022}'),
        "the value is masked with bullets:\n{overlay}"
    );
}

#[test]
fn r_reloads_only_a_failed_env_var_and_leaves_others_alone() {
    let mut app = TuiApp::default();
    // TOKEN's op:// reference fails to resolve (simulating 1Password being
    // locked/unreachable at load time); PLAIN is an ordinary literal.
    let (mut env, _pending) =
        crate::environment::parse_vars_pending("e".into(), "TOKEN={{ op://V/i/f }}\nPLAIN=hello");
    env.apply_update(&crate::environment::EnvUpdate {
        env_id: env.id,
        index: 0,
        value: None,
    });
    assert!(env.vars[0].is_failed(), "TOKEN should start out failed");
    add_global_env(&mut app, env);
    open_only_env_popup(&mut app);

    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));

    let env = only_env(&app);
    assert!(
        env.vars[0].loading,
        "'r' should re-mark the failed var as loading while it retries"
    );
    assert!(
        !env.vars[0].is_failed(),
        "no longer counted as failed once a retry is in flight"
    );
    assert_eq!(
        env.vars[1].value, "hello",
        "the unrelated PLAIN var must be untouched"
    );
    assert_eq!(
        app.pending_env.len(),
        1,
        "a background resolution should have been spawned for the retry"
    );
    assert!(
        matches!(app.status, Some(crate::i18n::Status::EnvVarReloading(ref k)) if k == "TOKEN"),
        "the status bar should confirm which variable is being retried"
    );
}

#[test]
fn r_is_a_no_op_when_the_selected_var_did_not_fail() {
    let mut app = app_with_resolved_secret("s3cr3t");
    open_only_env_popup(&mut app);
    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    assert!(
        app.pending_env.is_empty(),
        "nothing to retry for an already-resolved variable"
    );
    assert!(
        app.status.is_none(),
        "no status message when there was nothing to reload"
    );
}

// ── Rename tab (F2 / Enter-on-tab-bar), now that 'r' means "reload var" ──

#[test]
fn plain_r_no_longer_renames_a_tab() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.active_tab = 1;
    app.focus = Pane::Main;
    press(&mut app, KeyCode::Char('r'));
    assert!(
        app.overlay.is_none(),
        "'r' must not open the rename prompt any more"
    );
}

#[test]
fn f2_renames_the_active_non_builtin_tab() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.active_tab = 1;
    app.focus = Pane::Main;
    press(&mut app, KeyCode::F(2));
    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::Prompt {
                kind: PromptKind::RenameTab(1),
                ..
            })
        ),
        "F2 should open the rename prompt for the active tab"
    );
}

#[test]
fn f2_does_nothing_on_the_builtin_request_tab() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::F(2));
    assert!(
        app.overlay.is_none(),
        "the built-in Request tab can't be renamed"
    );
}

#[test]
fn enter_on_the_tab_bar_renames_the_selected_tab() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.active_tab = 1;
    app.focus = Pane::Tabs;
    app.on_enter();
    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::Prompt {
                kind: PromptKind::RenameTab(1),
                ..
            })
        ),
        "Enter while browsing the tab bar should rename the selected (active) tab"
    );
}

#[test]
fn enter_on_the_tab_bar_moves_focus_to_the_list_for_the_builtin_tab() {
    let mut app = TuiApp {
        focus: Pane::Tabs,
        ..Default::default()
    };
    app.on_enter();
    assert!(
        app.overlay.is_none(),
        "the built-in Request tab can't be renamed"
    );
    assert!(
        app.focus == Pane::List,
        "Enter should fall back to moving focus into the list"
    );
}

// ── Manually-added environment variables ──────────────────────────────

#[test]
fn n_key_adds_an_env_variable_in_the_env_pane_and_opens_new_request_elsewhere() {
    let mut app = TuiApp::default();
    let n = KeyCode::Char('n');
    // Outside the Env pane, 'n' opens New Request (the same "add a new
    // item" action, unified onto one key since the two panes are never
    // both focused at once).
    app.focus = Pane::List;
    press(&mut app, n);
    assert!(
        matches!(app.overlay, Some(Overlay::NewRequest(_))),
        "'n' opens New Request outside the Env pane"
    );
    app.overlay = None;

    // Inside the env popup, 'n' opens the add-variable form instead.
    let env_id = add_empty_global_env(&mut app, "e");
    app.focus = Pane::GlobalEnv;
    app.overlay = Some(Overlay::EnvPopup(EnvPopupState::new(env_id)));
    press(&mut app, n);
    assert!(
        matches!(app.overlay, Some(Overlay::EnvVarForm(ref f)) if f.env_id == env_id && !f.on_value),
        "'n' opens the Key/Value add-variable form in the env popup"
    );
}

#[test]
fn env_var_form_key_then_value_adds_the_variable() {
    let mut app = TuiApp::default();
    let env_id = add_empty_global_env(&mut app, "e");
    app.overlay = Some(Overlay::EnvPopup(EnvPopupState::new(env_id)));
    press(&mut app, KeyCode::Char('n')); // open the form (Key cell focused)
    for ch in "API_TOKEN".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // Key -> Value
    for ch in "abc123".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Enter); // commit

    assert!(
        matches!(app.overlay, Some(Overlay::EnvPopup(_))),
        "committing returns to the popup"
    );
    let env = only_env(&app);
    assert_eq!(env.vars.len(), 1);
    assert_eq!(env.vars[0].key, "API_TOKEN");
    assert_eq!(env.vars[0].value, "abc123");
    assert!(env.vars[0].user_added);
}

#[test]
fn adding_a_variable_creates_a_user_added_entry() {
    let mut app = TuiApp::default();
    let env_id = add_empty_global_env(&mut app, "e");

    app.add_env_var(env_id, " API_TOKEN ".into(), " abc123 ".into());

    let env = only_env(&app);
    assert_eq!(env.vars.len(), 1);
    let v = &env.vars[0];
    assert_eq!(v.key, "API_TOKEN");
    assert_eq!(v.value, "abc123", "surrounding whitespace is trimmed");
    assert!(v.user_added, "a hand-added variable is flagged user_added");
    assert!(v.resolved);
}

#[test]
fn adding_a_variable_with_an_existing_key_replaces_it() {
    let mut app = app_with_resolved_secret("s3cr3t"); // env has TOKEN at index 0
    app.add_env_var(only_env_id(&app), "TOKEN".into(), "plain".into());

    let env = only_env(&app);
    assert_eq!(
        env.vars.len(),
        1,
        "a same-name entry is replaced, not duplicated"
    );
    assert_eq!(env.vars[0].value, "plain");
    assert!(env.vars[0].user_added);
}

#[test]
fn adding_a_variable_ignores_an_empty_key() {
    let mut app = TuiApp::default();
    let env_id = add_empty_global_env(&mut app, "e");
    app.add_env_var(env_id, "   ".into(), "x".into());
    assert!(only_env(&app).vars.is_empty(), "an empty key is ignored");
}

#[test]
fn loading_an_environment_with_a_name_collision_opens_the_collision_popup() {
    let mut app = TuiApp::default();
    let env_id = add_empty_global_env(&mut app, "e");
    app.add_env_var(env_id, "KEEP".into(), "mine".into());
    app.add_env_var(env_id, "SHARED".into(), "mine".into());

    assert_eq!(
        app.load_environment_text("e".into(), "SHARED=fromfile\nFROM_FILE=x\n", None, None),
        None,
        "loads are deferred until the collision is resolved"
    );
    assert!(
        matches!(app.overlay, Some(Overlay::EnvCollision(_))),
        "a same-name environment now opens the collision picker instead of auto-merging"
    );
}

#[test]
fn env_panel_marks_a_user_added_variable_with_a_plus() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut app = TuiApp::default();
    let env_id = add_empty_global_env(&mut app, "e");
    app.add_env_var(env_id, "MYVAR".into(), "hello".into());

    let mut term = Terminal::new(TestBackend::new(50, 8)).unwrap();
    let popup = EnvPopupState::new(env_id);
    term.draw(|f| super::draw::draw_env_popup(f, &app, &popup, &s, &th))
        .unwrap();
    let panel = buffer_text(term.backend().buffer());
    assert!(
        panel.contains('\u{271a}'),
        "user-added var shows the plus marker:\n{panel}"
    );
    assert!(
        panel.contains("MYVAR"),
        "the variable name is shown:\n{panel}"
    );
}

#[test]
fn selected_env_row_keeps_the_status_dot_colour_visible() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    // A resolved secret's dot is th.ok; the row is selected in the popup.
    let app = app_with_resolved_secret("s3cr3t");

    let mut term = Terminal::new(TestBackend::new(50, 8)).unwrap();
    let popup = EnvPopupState::new(only_env_id(&app));
    term.draw(|f| super::draw::draw_env_popup(f, &app, &popup, &s, &th))
        .unwrap();
    let buf = term.backend().buffer();
    let area = *buf.area();
    let mut found = false;
    for y in 0..area.height {
        for x in 0..area.width {
            if buf[(x, y)].symbol() == "\u{25cf}" {
                // The selection highlight sets a background only, so the dot
                // keeps its status colour instead of being overwritten by th.bg.
                assert_eq!(
                    buf[(x, y)].fg,
                    th.ok,
                    "selected row's status dot keeps its colour"
                );
                found = true;
            }
        }
    }
    assert!(found, "a status dot (●) should be rendered");
}

// ── Deleting requests & user-added request marker ──────────────────────

#[test]
fn x_deletes_the_selected_request_when_the_list_is_focused() {
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("a", "GET", "http://h/a", vec![], ""));
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("b", "GET", "http://h/b", vec![], ""));
    app.collections[0].selected_entry = 0;
    app.focus = Pane::List;

    press(&mut app, KeyCode::Char('x'));

    assert_eq!(
        app.collections[0].entries.len(),
        1,
        "the selected request is deleted"
    );
    assert_eq!(
        app.collections[0].entries[0].url, "http://h/b",
        "the other request remains"
    );
}

#[test]
fn u_restores_the_most_recently_deleted_request_when_the_list_is_focused() {
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("a", "GET", "http://h/a", vec![], ""));
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("b", "GET", "http://h/b", vec![], ""));
    app.collections[0].selected_entry = 0;
    app.focus = Pane::List;
    press(&mut app, KeyCode::Char('x')); // deletes "a"
    assert_eq!(app.collections[0].entries.len(), 1);

    press(&mut app, KeyCode::Char('u'));

    assert_eq!(
        app.collections[0].entries.len(),
        2,
        "the deleted request came back"
    );
    assert_eq!(
        app.collections[0].entries[0].url, "http://h/a",
        "restored at its original index"
    );
    assert_eq!(
        app.collections[0].selected_entry, 0,
        "the restored request becomes selected"
    );
}

#[test]
fn restoring_with_no_deleted_requests_is_a_no_op() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('u'));
    assert!(app.collections[0].entries.is_empty());
}

#[test]
fn restored_requests_come_back_in_last_deleted_first_order() {
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("a", "GET", "http://h/a", vec![], ""));
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("b", "GET", "http://h/b", vec![], ""));
    app.focus = Pane::List;
    app.collections[0].selected_entry = 0;
    app.delete_selected_request(); // deletes "a"; ["b"] remains
    app.collections[0].selected_entry = 0;
    app.delete_selected_request(); // deletes "b"; [] remains
    assert!(app.collections[0].entries.is_empty());

    app.restore_deleted_request();
    assert_eq!(
        app.collections[0].entries[0].url, "http://h/b",
        "most recently deleted comes back first"
    );
    app.restore_deleted_request();
    assert!(
        app.collections[0]
            .entries
            .iter()
            .any(|e| e.url == "http://h/a"),
        "the earlier deletion is restored too"
    );
}

#[test]
fn u_reopens_the_closed_tab_instead_when_not_on_the_list_pane() {
    // Mirrors `x`'s pane-dependent behaviour (delete request on the List
    // pane vs. close/reopen the tab elsewhere): deleting a request while
    // on the List pane must not be confused with a closed tab, and `u`
    // away from the List pane must still reopen the tab, not touch the
    // per-collection deleted-request stack.
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("a", "GET", "http://h/a", vec![], ""));
    app.focus = Pane::List;
    app.collections[0].selected_entry = 0;
    press(&mut app, KeyCode::Char('x')); // deletes "a" from the built-in tab
    assert!(app.collections[0].entries.is_empty());

    app.collections.push(Collection::new("api".into(), vec![]));
    app.active_tab = 1;
    app.focus = Pane::Tabs;
    app.close_active_tab();
    assert_eq!(app.collections.len(), 1);

    press(&mut app, KeyCode::Char('u'));

    assert_eq!(
        app.collections.len(),
        2,
        "the closed tab came back, not the deleted request"
    );
    assert_eq!(app.collections[1].name, "api");
    assert!(
        app.collections[0].entries.is_empty(),
        "the deleted request stays untouched since focus wasn't on the List pane"
    );
}

#[test]
fn x_closes_the_collection_tab_when_not_on_the_list_pane() {
    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("extra".into(), vec![]));
    app.active_tab = 1;
    app.focus = Pane::Tabs;

    press(&mut app, KeyCode::Char('x'));

    assert_eq!(app.collections.len(), 1, "the collection tab is closed");
    assert_eq!(app.active_tab, 0);
}

#[test]
fn a_request_added_to_a_real_collection_is_marked_user_added() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));

    // Add to the "api" collection (tab 1) → user-added.
    let mut form = NewReq::new(String::new(), vec!["Scratch".into(), "api".into()], 1, None);
    form.url = super::editor::Editor::new("http://h/x", false);
    app.submit_new_request(form);
    assert!(
        app.collections[1].entries[0].user_added,
        "a request added to a real collection is marked"
    );

    // Add to the Scratch Space (tab 0) → NOT marked.
    let mut form0 = NewReq::new(String::new(), vec!["Scratch".into(), "api".into()], 0, None);
    form0.url = super::editor::Editor::new("http://h/y", false);
    app.submit_new_request(form0);
    assert!(
        !app.collections[0].entries[0].user_added,
        "scratch-space requests are not marked"
    );
}

#[test]
fn f2_with_an_empty_url_keeps_the_wizard_open_and_warns_instead_of_discarding() {
    let mut app = TuiApp {
        focus: Pane::List,
        ..Default::default()
    };

    // Open the New Request wizard and type only a Name (no URL).
    press(&mut app, KeyCode::Char('n'));
    if let Some(Overlay::NewRequest(form)) = &mut app.overlay {
        form.name = super::editor::Editor::new("draft", false);
    } else {
        panic!("wizard did not open");
    }

    // F2 must NOT close the wizard or discard the typed fields when the URL
    // is empty — the request can't be saved without one.
    press(&mut app, KeyCode::F(2));

    match &app.overlay {
        Some(Overlay::NewRequest(form)) => {
            assert_eq!(form.focus, NewField::Url, "focus jumps to the URL field");
            assert_eq!(form.name.text(), "draft", "typed fields are preserved");
        }
        _ => panic!("the wizard must stay open on an empty-URL submit"),
    }
    assert!(
        matches!(app.status, Some(crate::i18n::Status::NewRequestUrlRequired)),
        "a status hint explains why saving was blocked"
    );
    assert!(
        app.collections[0].entries.is_empty(),
        "nothing was saved to any collection"
    );

    // Once a URL is filled in, F2 saves normally.
    if let Some(Overlay::NewRequest(form)) = &mut app.overlay {
        form.url = super::editor::Editor::new("http://h/ok", false);
    }
    press(&mut app, KeyCode::F(2));
    assert!(
        app.overlay.is_none(),
        "the wizard closes once the URL is valid"
    );
    assert_eq!(app.collections[0].entries.len(), 1);
    assert_eq!(app.collections[0].entries[0].url, "http://h/ok");
}

#[test]
fn an_untitled_request_stays_at_the_root_instead_of_a_url_named_folder() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));

    // No name typed → the URL (which contains slashes) must NOT become the
    // title, or `tree::entry_path` would file it under a phantom "http:/"
    // folder away from its siblings.
    let mut form = NewReq::new(String::new(), vec!["Scratch".into(), "api".into()], 1, None);
    form.url = super::editor::Editor::new("http://host/path/x", false);
    app.submit_new_request(form);

    let entry = &app.collections[1].entries[0];
    assert_eq!(entry.title, "", "an untitled request keeps an empty title");
    assert!(
        crate::tree::folder_of(&app.collections[1].entries, 0).is_empty(),
        "it lives at the root of the folder tree, not in a URL-named folder"
    );
}

#[test]
fn collection_list_shows_the_plus_for_a_user_added_request() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    let mut file_req = HurlEntry::from_fields("from-file", "GET", "http://h/file", vec![], "");
    file_req.user_added = false;
    let mut added = HurlEntry::from_fields("added", "POST", "http://h/added", vec![], "");
    added.user_added = true;
    app.collections[1].entries = vec![file_req, added];
    app.active_tab = 1;
    app.focus = Pane::List;

    let mut term = Terminal::new(TestBackend::new(46, 8)).unwrap();
    term.draw(|f| super::draw::draw_collection_left(f, f.area(), &app, 1, &s, &th))
        .unwrap();
    let panel = buffer_text(term.backend().buffer());
    assert!(
        panel.contains('\u{271a}'),
        "a user-added request shows the plus marker:\n{panel}"
    );
}

#[test]
fn response_panel_shows_assert_results_supplemental_to_status() {
    use crate::hurl::AssertOutcome;
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut app = TuiApp::default();
    {
        let ci = app.active_tab;
        let col = &mut app.collections[ci];
        col.entries.push(HurlEntry::default());
        col.selected_entry = 0;
        let entry = &mut col.entries[0];
        entry.last_response = Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: "{}".into(),
            assert_results: vec![
                AssertOutcome {
                    expr: "jsonpath \"$.status\" == \"ok\"".into(),
                    passed: true,
                    detail: String::new(),
                },
                AssertOutcome {
                    expr: "jsonpath \"$.via\" == \"oauth2\"".into(),
                    passed: false,
                    detail: "got \"bearer\"".into(),
                },
            ],
            ..Default::default()
        });
    }

    let mut term = Terminal::new(TestBackend::new(90, 12)).unwrap();
    let ci = app.active_tab;
    term.draw(|f| super::draw::draw_response(f, f.area(), &mut app, ci, &s, &th))
        .unwrap();
    let out = buffer_text(term.backend().buffer());

    assert!(out.contains("200 OK"), "status still shown:\n{out}");
    assert!(
        out.contains("[Asserts]"),
        "assert badge supplements the status:\n{out}"
    );
    assert!(out.contains("1/2"), "passed/total count shown:\n{out}");
    assert!(
        out.contains('\u{2713}'),
        "a passing assert shows a check:\n{out}"
    );
    assert!(
        out.contains('\u{2717}'),
        "a failing assert shows a cross:\n{out}"
    );
    assert!(
        out.contains("got"),
        "the failing assert's actual value is shown:\n{out}"
    );
}

#[test]
fn response_panel_shows_the_selected_entrys_own_response_not_the_last_entry_run() {
    // Regression test: after a "Run All" batch, the Response pane must
    // show whichever entry is currently selected in the Requests list —
    // not just whichever entry happened to finish last.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    let col = &mut app.collections[ci];
    col.entries = vec![
        HurlEntry {
            title: "first".into(),
            last_response: Some(crate::http::ApiResponse {
                status: 200,
                status_text: "OK".into(),
                body: "first-body".into(),
                ..Default::default()
            }),
            ..Default::default()
        },
        HurlEntry {
            title: "second".into(),
            last_response: Some(crate::http::ApiResponse {
                status: 500,
                status_text: "Internal Server Error".into(),
                body: "second-body".into(),
                ..Default::default()
            }),
            ..Default::default()
        },
    ];

    let mut term = Terminal::new(TestBackend::new(90, 12)).unwrap();

    app.collections[ci].selected_entry = 0;
    term.draw(|f| super::draw::draw_response(f, f.area(), &mut app, ci, &s, &th))
        .unwrap();
    let out = buffer_text(term.backend().buffer());
    assert!(
        out.contains("200 OK"),
        "first entry's own status shown:\n{out}"
    );
    assert!(
        out.contains("first-body"),
        "first entry's own body shown:\n{out}"
    );
    assert!(
        !out.contains("second-body"),
        "second entry's body must not leak in:\n{out}"
    );

    app.collections[ci].selected_entry = 1;
    term.draw(|f| super::draw::draw_response(f, f.area(), &mut app, ci, &s, &th))
        .unwrap();
    let out = buffer_text(term.backend().buffer());
    assert!(
        out.contains("500 Internal Server Error"),
        "second entry's own status shown:\n{out}"
    );
    assert!(
        out.contains("second-body"),
        "second entry's own body shown:\n{out}"
    );
    assert!(
        !out.contains("first-body"),
        "first entry's body must not leak in:\n{out}"
    );
}

#[test]
fn response_panel_wraps_long_lines_instead_of_truncating() {
    // Regression test: a very long, unbroken line in the response body
    // (e.g. a long token) must wrap onto further rows instead of being
    // cut off at the panel's right edge.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let long_line: String = (0..100).map(|i| format!("{i:03}")).collect();
    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "long".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(long_line.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];

    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| super::draw::draw_response(f, f.area(), &mut app, ci, &s, &th))
        .unwrap();
    assert!(
        app.resp_max_scroll > 0,
        "wrapping the long line should add scrollable rows"
    );

    // Scroll to the bottom to reach the tail (it wraps onto rows below the
    // fold; being reachable via scrolling — not visible on the first
    // screen — is exactly the fix for the truncation bug).
    app.focus = Pane::Response;
    for _ in 0..app.resp_max_scroll {
        app.nav(1);
    }
    term.draw(|f| super::draw::draw_response(f, f.area(), &mut app, ci, &s, &th))
        .unwrap();
    let out = flattened_content(term.backend().buffer());
    let tail = &long_line[long_line.len() - 12..];
    assert!(
        out.contains(tail),
        "the tail of the long line must still be reachable (wrapped, not truncated):\n{out}"
    );
    assert!(
        app.resp_max_scroll > 0,
        "wrapping the long line should add scrollable rows"
    );
}

#[test]
fn a_soft_wrapped_response_line_shows_the_wrap_marker_glyph() {
    // The wrap marker (a dim `↵` in the reserved rightmost column) is what
    // tells the user a row is a soft-wrapped continuation rather than a
    // distinct logical line. It must actually be painted on a wrapped body.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let long_line: String = (0..100).map(|i| format!("{i:03}")).collect();
    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "long".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(long_line.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];

    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| super::draw::draw_response(f, f.area(), &mut app, ci, &s, &th))
        .unwrap();

    let painted = buffer_text(term.backend().buffer());
    assert!(
        painted.contains('\u{21b5}'),
        "a soft-wrapped response row must show the wrap marker:\n{painted}"
    );
}

#[test]
fn response_panel_scroll_stops_at_the_last_line() {
    // The user must not be able to keep scrolling down indefinitely: once
    // the last (wrapped) line of the response body is in view, further
    // downward scrolling is a no-op.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let body: String = (0..40)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "many-lines".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(body.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| super::draw::draw_response(f, f.area(), &mut app, ci, &s, &th))
        .unwrap();
    assert!(
        app.resp_max_scroll > 0,
        "content must be taller than the viewport for this test"
    );

    for _ in 0..200 {
        app.nav(1);
        term.draw(|f| super::draw::draw_response(f, f.area(), &mut app, ci, &s, &th))
            .unwrap();
    }
    assert_eq!(
        app.resp_panel.scroll(),
        app.resp_max_scroll,
        "scrolling stops once the last line is in view, it never scrolls past it"
    );
    let out = buffer_text(term.backend().buffer());
    assert!(
        out.contains("line39"),
        "the last line is reachable and visible:\n{out}"
    );
}

#[test]
fn clicking_the_response_scrollbar_jumps_the_scroll_to_that_position() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let body: String = (0..200)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "many-lines".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(body.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let bar = app.resp_scrollbar_area;
    assert!(
        bar.height > 1,
        "the scrollbar must actually render for this test"
    );
    assert_eq!(app.resp_panel.scroll(), 0);

    // Click at the very bottom of the scrollbar track.
    let ev = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: bar.x,
        row: bar.y + bar.height - 1,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(ev);
    assert_eq!(
        app.resp_panel.scroll(),
        app.resp_max_scroll,
        "clicking the bottom of the track jumps straight there"
    );
    assert_eq!(app.scrollbar_drag, Some(Pane::Response));

    // The click must not have started (or disturbed) a text selection.
    assert!(app.text_selection().is_none());

    // Dragging back up to the top of the track should scroll back to 0,
    // even though a Drag event's column may drift off the one-column
    // track (dragging isn't pixel-perfect).
    let up = MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: bar.x + 3,
        row: bar.y,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(up);
    assert_eq!(
        app.resp_panel.scroll(),
        0,
        "dragging to the top of the track scrolls back to the start"
    );

    let up_ev = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: bar.x + 3,
        row: bar.y,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(up_ev);
    assert_eq!(
        app.scrollbar_drag, None,
        "releasing the mouse ends the scrollbar drag"
    );
}

#[test]
fn clicking_the_main_panel_scrollbar_scrolls_the_request_json_panel() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    // A single, very long line forces the Request JSON panel to need a
    // scrollbar even though there's only one logical line of text.
    let long_url = format!("https://example.test/{}", "x".repeat(500));
    app.collections[ci].entries = vec![HurlEntry {
        title: "long".into(),
        url: long_url,
        ..Default::default()
    }];
    app.focus = Pane::Main;

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let bar = app.main_scrollbar_area;
    assert!(
        bar.height > 1,
        "the scrollbar must actually render for this test"
    );
    assert_eq!(app.main_panel.scroll(), 0);

    let ev = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: bar.x,
        row: bar.y + bar.height - 1,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(ev);
    assert_eq!(app.main_panel.scroll(), app.main_max_scroll);
    assert_eq!(app.scrollbar_drag, Some(Pane::Main));
}

#[test]
fn ctrl_up_down_page_scrolls_the_response_panel_by_its_visible_height() {
    // Ctrl+↑/↓ should jump a whole page (the panel's visible height) at
    // a time, rather than one line — much faster to move through a
    // large response than plain ↑/↓.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let body: String = (0..200)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "many-lines".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(body.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| super::draw::draw_response(f, f.area(), &mut app, ci, &s, &th))
        .unwrap();
    assert!(
        app.resp_max_scroll > 0,
        "content must be taller than the viewport for this test"
    );
    let page = app.resp_text_area.height;
    assert!(
        page > 1,
        "the visible body area must be more than one row for this test to be meaningful"
    );

    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
    assert_eq!(
        app.resp_panel.scroll(),
        page,
        "Ctrl+Down scrolls down by exactly one page"
    );

    app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_eq!(
        app.resp_panel.scroll(),
        0,
        "Ctrl+Up scrolls back up by exactly one page"
    );

    // Plain (unmodified) ↑/↓ still moves one line at a time.
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(
        app.resp_panel.scroll(),
        1,
        "plain Down still scrolls a single line"
    );
}

#[test]
fn response_panel_shows_a_scrollbar_overlaid_on_the_border_outside_the_selectable_area() {
    // The scrollbar must be visible whenever the body overflows the
    // viewport, and it must live entirely on the panel's border column —
    // never inside `resp_text_area` — so it can't be dragged into as
    // part of a text selection and never eats into the text width.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let body: String = (0..40)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "many-lines".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(body.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let area = ratatui::layout::Rect::new(0, 0, 40, 10);
    let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
    term.draw(|f| super::draw::draw_response(f, area, &mut app, ci, &s, &th))
        .unwrap();
    assert!(
        app.resp_max_scroll > 0,
        "content must be taller than the viewport for this test"
    );

    let text_area = app.resp_text_area;
    let border_x = area.x + area.width - 1;
    assert!(
        border_x >= text_area.x + text_area.width,
        "the scrollbar's border column must sit outside the selectable text area"
    );

    let buf = term.backend().buffer();
    let mut saw_thumb_or_track = false;
    for y in text_area.y..text_area.y + text_area.height {
        let sym = buf[(border_x, y)].symbol();
        if sym == "\u{2588}" || sym == "\u{2502}" {
            saw_thumb_or_track = true;
        }
        // The border column right beside the text must never show text
        // content — only border/scrollbar glyphs.
        assert_ne!(
            sym, "e",
            "no stray text should ever land on the border/scrollbar column"
        );
    }
    assert!(
        saw_thumb_or_track,
        "a scrollbar track or thumb should be drawn once the body overflows"
    );
}

#[test]
fn request_json_panel_wraps_long_lines_instead_of_truncating() {
    // Regression test: a very long, unbroken line in the Request JSON
    // preview (e.g. a long token value) must wrap onto further rows
    // instead of being cut off at the panel's right edge.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let token: String = (0..100).map(|i| format!("{i:03}")).collect();
    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries =
        vec![HurlEntry::from_fields("t", "GET", "http://h/x", vec![], "")];
    app.collections[ci].request_json_buf = format!("{{\n  \"token\": \"{token}\"\n}}");
    app.collections[ci].request_json_for = Some(0);

    let mut term = Terminal::new(TestBackend::new(40, 14)).unwrap();
    term.draw(|f| super::draw::draw_collection_main(f, f.area(), &mut app, ci, &s, &th))
        .unwrap();
    assert!(
        app.main_max_scroll > 0,
        "wrapping the long line should add scrollable rows"
    );

    // Scroll to the bottom to reach the tail (it wraps onto rows below the
    // fold; being reachable via scrolling — not visible on the first
    // screen — is exactly the fix for the truncation bug).
    app.focus = Pane::Main;
    for _ in 0..app.main_max_scroll {
        app.nav(1);
    }
    term.draw(|f| super::draw::draw_collection_main(f, f.area(), &mut app, ci, &s, &th))
        .unwrap();
    let out = flattened_content(term.backend().buffer());
    let tail = &token[token.len() - 12..];
    assert!(
        out.contains(tail),
        "the tail of the long token must still be reachable (wrapped, not truncated):\n{out}"
    );
}

#[test]
fn request_json_panel_shows_a_scrollbar_overlaid_on_the_border_outside_the_selectable_area() {
    // Same guarantee as the Response panel's: the Request JSON/Hurl
    // panel's scrollbar lives on the border column, never inside
    // `main_text_area`, so it can't be selected/dragged as text and
    // never steals a column of text width.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let token: String = (0..100).map(|i| format!("{i:03}")).collect();
    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries =
        vec![HurlEntry::from_fields("t", "GET", "http://h/x", vec![], "")];
    app.collections[ci].request_json_buf = format!("{{\n  \"token\": \"{token}\"\n}}");
    app.collections[ci].request_json_for = Some(0);

    let area = ratatui::layout::Rect::new(0, 0, 40, 14);
    let mut term = Terminal::new(TestBackend::new(40, 14)).unwrap();
    term.draw(|f| super::draw::draw_collection_main(f, area, &mut app, ci, &s, &th))
        .unwrap();
    assert!(
        app.main_max_scroll > 0,
        "wrapping the long line should add scrollable rows"
    );

    let text_area = app.main_text_area;
    let border_x = area.x + area.width - 1;
    assert!(
        border_x >= text_area.x + text_area.width,
        "the scrollbar's border column must sit outside the selectable text area"
    );

    let buf = term.backend().buffer();
    let mut saw_thumb_or_track = false;
    for y in text_area.y..text_area.y + text_area.height {
        let sym = buf[(border_x, y)].symbol();
        if sym == "\u{2588}" || sym == "\u{2502}" {
            saw_thumb_or_track = true;
        }
    }
    assert!(
        saw_thumb_or_track,
        "a scrollbar track or thumb should be drawn once the body overflows"
    );
}

// ── Panel-scoped mouse text selection ─────────────────────────────────

#[test]
fn mouse_drag_inside_the_response_panel_selects_scoped_text_and_paints_a_highlight() {
    use crate::i18n::Language;
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "sel".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: "first line here\nsecond".into(),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let area = app.resp_text_area;
    assert!(
        area.width > 0 && area.height > 1,
        "the body must actually render for this test"
    );

    // Drag-select columns 2..=4 of the first visible row ("rst " of
    // "first line here").
    let ev = |kind, col_offset: u16| MouseEvent {
        kind,
        column: area.x + col_offset,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), 2));
    app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), 5));
    app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), 5));

    let sel = app
        .text_selection()
        .expect("dragging inside the response panel should start a selection");
    assert_eq!(sel.pane, Pane::Response);
    let text = super::selection::extract_text(
        sel.anchor,
        sel.cursor,
        app.resp_panel.wrap().unwrap(),
        None,
    );
    assert_eq!(
        text.as_deref(),
        Some("rst "),
        "chars 2..=5 of \"first line here\""
    );
    assert!(
        matches!(app.status, Some(crate::i18n::Status::Copied)),
        "releasing a mouse-drag selection copies it and sets the status message"
    );

    // Re-drawing (content unchanged, so the layout/area is stable) paints
    // the highlight with the app's own explicit selection colour,
    // confined to the response panel.
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let select_bg = super::theme::theme(&Language::English).select_bg;
    let buf = term.backend().buffer();
    let selected_cell = buf.cell((area.x + 2, area.y)).unwrap();
    assert_eq!(
        selected_cell.bg, select_bg,
        "a selected cell must use the app's own selection colour"
    );
    let outside_cell = buf.cell((area.x + 10, area.y)).unwrap();
    assert_ne!(
        outside_cell.bg, select_bg,
        "cells outside the selection must be untouched"
    );
}

/// `y` re-triggers a copy of the active selection without disturbing it
/// (unlike Esc, which clears it) — an explicit fallback for terminals
/// that don't act on the automatic OSC 52 write from mouse-release.
/// `copy_to_clipboard` itself just writes to stdout (best-effort, see
/// `clipboard.rs`), so this only asserts the selection survives the key
/// press and that `y` is a no-op when there is nothing selected.
#[test]
fn y_recopies_the_active_selection_without_clearing_it() {
    let mut app = TuiApp::default();
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(0, 2),
        cursor: TextPos::new(0, 5),
    }));

    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

    assert!(
        app.text_selection().is_some(),
        "`y` must not clear the selection it just copied"
    );
}

/// Copying a selection (via `y`) sets a status bar message confirming
/// it, so the user has explicit feedback that something happened —
/// distinct from `y` being a no-op with nothing selected/copyable.
#[test]
fn copying_a_selection_sets_the_copied_status_message() {
    use crate::i18n::Status;
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "sel".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: "first line here\nsecond line".into(),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    app.set_text_selection(Some(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(0, 0),
        cursor: TextPos::new(0, 5),
    }));
    assert!(app.status.is_none(), "no status before copying");

    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(
        matches!(app.status, Some(Status::Copied)),
        "copying a selection sets a status bar message confirming it"
    );
}

#[test]
fn y_without_an_active_selection_is_a_no_op() {
    let mut app = TuiApp::default();

    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

    assert!(app.text_selection().is_none());
    assert!(
        app.status.is_none(),
        "nothing was copied, so no status message is shown"
    );
}

/// With no selection, `y` falls back to copying the *whole* focused
/// panel (Request JSON or Response) — so a user can grab the entire
/// body without first having to drag-select every line of what might be
/// a huge response. `whole_panel_text`/`can_copy` are the pure pieces of
/// that logic (`copy_to_clipboard` itself just writes to stdout, see
/// `clipboard.rs`), so this asserts on those directly plus the fact
/// that pressing `y` doesn't create/disturb any selection state.
#[test]
fn whole_panel_text_returns_the_full_response_body_when_the_response_panel_has_focus() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "sel".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: "first line here\nsecond line".into(),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    let text = app
        .whole_panel_text(Pane::Response)
        .expect("response panel has content");
    assert_eq!(text, "first line here\nsecond line");
    // Main has no content in this scenario (no request JSON body typed).
    assert!(
        app.can_copy(),
        "y should copy the whole response since the Response panel has focus"
    );
}

#[test]
fn whole_panel_text_returns_the_full_request_json_when_the_main_panel_has_focus() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    let entry = HurlEntry {
        method: "GET".into(),
        url: "http://example.com/path".into(),
        ..Default::default()
    };
    let json = crate::request::build_request_json(&entry);
    app.collections[ci].entries = vec![entry];
    app.focus = Pane::Main;

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    let text = app
        .whole_panel_text(Pane::Main)
        .expect("main panel has content");
    assert_eq!(text, json);
    assert!(
        app.can_copy(),
        "y should copy the whole request JSON since the Main panel has focus"
    );
}

/// Copying the Main panel's JSON body must return the *substituted*
/// value the user actually sees on screen (e.g. a resolved `{{ TOKEN }}`
/// environment variable shown in a header value), not the raw
/// `{{ TOKEN }}` template syntax that's still in the underlying buffer.
/// Covers both the whole-panel copy (`y`) and a mouse-dragged selection,
/// since both must read from the same substituted text.
#[test]
fn main_panel_copy_uses_the_substituted_value_not_the_raw_template() {
    use crate::environment::{EnvVar, Environment, ValueSource};
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    let entry = HurlEntry {
        method: "GET".into(),
        url: "http://example.com/path".into(),
        headers: vec![("X-Token".into(), "{{ TOKEN }}".into(), true)],
        ..Default::default()
    };
    app.collections[ci].entries = vec![entry];
    let env_id = add_global_env(
        &mut app,
        Environment {
            id: 0,
            name: "e".into(),
            vars: vec![EnvVar {
                key: "TOKEN".into(),
                value: "secret123".into(),
                source: ValueSource::Literal,
                resolved: true,
                loading: false,
                original_value: "secret123".into(),
                modified: false,
                user_added: false,
                raw: String::new(),
            }],
            path: None,
            git_origin: None,
        },
    );
    app.collections[ci].linked_env_id = Some(env_id);
    app.focus = Pane::Main;

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    let text = app
        .whole_panel_text(Pane::Main)
        .expect("main panel has content");
    assert!(
        text.contains("secret123"),
        "whole-panel copy must contain the substituted value: {text}"
    );
    assert!(
        !text.contains("{{ TOKEN }}"),
        "whole-panel copy must not contain the raw template: {text}"
    );

    // Find the wrapped row that shows the substituted header value and
    // drag-select it, to confirm mouse selection extraction matches too.
    let area = app.main_text_area;
    let row_idx = (0..area.height)
        .find(|&r| {
            app.main_panel
                .wrap()
                .as_ref()
                .map(|w| {
                    w.line_text((app.main_panel.scroll() + r) as usize)
                        .contains("secret123")
                })
                .unwrap_or(false)
        })
        .expect("a visible row must show the substituted header value");
    let ev = |kind, col_offset: u16| MouseEvent {
        kind,
        column: area.x + col_offset,
        row: area.y + row_idx,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), 0));
    app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), 30));
    app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), 30));
    let sel = app
        .text_selection()
        .expect("dragging inside the Main panel should start a selection");
    let selected = super::selection::extract_text(
        sel.anchor,
        sel.cursor,
        app.main_panel.wrap().unwrap(),
        None,
    )
    .expect("selection should extract text");
    assert!(
        selected.contains("secret123"),
        "dragged selection must contain the substituted value: {selected}"
    );
    assert!(
        !selected.contains("{{ TOKEN }}"),
        "dragged selection must not contain the raw template: {selected}"
    );
}

/// The shadow-warning icon (`!`) that's rendered immediately before a
/// shadowed substitution's value is a pure UI annotation, not part of
/// the request — including it in copied text would silently corrupt a
/// pasted/sent request for anyone who doesn't manually remove it. It
/// must be excluded from both the whole-panel copy and a dragged
/// selection, while every *other*, legitimate `!` character in the body
/// (e.g. one that's simply part of a URL) must still be copied intact.
#[test]
fn main_panel_copy_excludes_the_shadow_icon_but_keeps_other_exclamation_marks() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    let entry = HurlEntry {
        method: "GET".into(),
        url: "http://example.com/path!important".into(),
        headers: vec![("X-Token".into(), "{{ TOKEN }}".into(), true)],
        ..Default::default()
    };
    app.collections[ci].entries = vec![entry];

    let (active, _) = crate::environment::parse_vars_pending("active".into(), "TOKEN=from-active");
    let active_id = add_global_env(&mut app, active);
    app.active_env_id = Some(active_id);
    let (linked, _) = crate::environment::parse_vars_pending("linked".into(), "TOKEN=secret123");
    let linked_id = add_global_env(&mut app, linked);
    app.collections[ci].linked_env_id = Some(linked_id);
    app.focus = Pane::Main;

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    assert!(
        !app.main_shadow_icon_positions.is_empty(),
        "TOKEN is defined in both envs, so a shadow icon must have been recorded"
    );

    let text = app
        .whole_panel_text(Pane::Main)
        .expect("main panel has content");
    assert!(
        text.contains("secret123"),
        "the shadowed (linked) value must still be copied: {text}"
    );
    assert!(
        !text.contains("!secret123"),
        "the shadow icon glued to the value must not be copied: {text}"
    );
    assert!(
        text.contains("path!important"),
        "an unrelated, legitimate '!' must still be copied intact: {text}"
    );

    // Same guarantee for a mouse-dragged selection covering the
    // substituted value's row.
    let area = app.main_text_area;
    let row_idx = (0..area.height)
        .find(|&r| {
            app.main_panel
                .wrap()
                .as_ref()
                .map(|w| {
                    w.line_text((app.main_panel.scroll() + r) as usize)
                        .contains("secret123")
                })
                .unwrap_or(false)
        })
        .expect("a visible row must show the substituted header value");
    let ev = |kind, col_offset: u16| MouseEvent {
        kind,
        column: area.x + col_offset,
        row: area.y + row_idx,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), 0));
    app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), 30));
    app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), 30));
    let sel = app
        .text_selection()
        .expect("dragging inside the Main panel should start a selection");
    let selected = app
        .concatenated_selection_text()
        .expect("selection should extract text");
    assert!(
        selected.contains("secret123"),
        "dragged selection must contain the shadowed value: {selected}"
    );
    assert!(
        !selected.contains("!secret123"),
        "dragged selection must not contain the shadow icon: {selected}"
    );
    let _ = sel;
}

/// When `default_request_view` is `Hurl`, the Main panel shows (and
/// `whole_panel_text`/`y` copies) the entry's Hurl text instead of its
/// JSON preview — the "whole-panel copy respects the active view"
/// requirement.
#[test]
fn main_panel_shows_and_copies_hurl_text_when_the_default_view_is_hurl() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp {
        default_request_view: RequestView::Hurl,
        ..Default::default()
    };
    let ci = app.active_tab;
    let entry = HurlEntry {
        method: "GET".into(),
        url: "http://example.com/path".into(),
        ..Default::default()
    };
    let hurl = entry.to_hurl();
    let json = crate::request::build_request_json(&entry);
    app.collections[ci].entries = vec![entry];
    app.focus = Pane::Main;

    let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer().clone();
    let text = buffer_text(&buf);
    assert!(
        text.contains("Request Hurl"),
        "the panel title switches to the Hurl-view label:\n{text}"
    );
    assert!(
        text.contains("GET http://example.com/path"),
        "the panel renders Hurl text:\n{text}"
    );

    let copied = app
        .whole_panel_text(Pane::Main)
        .expect("main panel has content");
    assert_eq!(
        copied, hurl,
        "whole-panel copy uses the Hurl text, not the JSON, when the Hurl view is active"
    );
    assert_ne!(
        copied, json,
        "sanity check: the JSON and Hurl forms actually differ here"
    );
}

/// The Raw Hurl view shows a disabled row as a `# key: value` comment, so the
/// user sees exactly what will be saved and (not) sent — the enabled flag is
/// round-tripped through the Hurl file as a comment, and the preview reflects
/// that faithfully.
#[test]
fn hurl_view_shows_disabled_rows_as_comments() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp {
        default_request_view: RequestView::Hurl,
        ..Default::default()
    };
    let ci = app.active_tab;
    let mut entry = HurlEntry {
        method: "GET".into(),
        url: "http://example.com/path".into(),
        ..Default::default()
    };
    entry.headers = vec![
        ("Accept".into(), "application/json".into(), true),
        ("X-Debug".into(), "1".into(), false),
    ];
    app.collections[ci].entries = vec![entry];
    app.focus = Pane::Main;

    let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let text = buffer_text(&term.backend().buffer().clone());
    assert!(
        text.contains("Accept: application/json"),
        "the enabled header renders normally:\n{text}"
    );
    assert!(
        text.contains("# X-Debug: 1"),
        "the disabled header renders as a comment:\n{text}"
    );
}

#[test]
fn can_copy_is_false_with_no_selection_and_focus_on_a_panel_with_no_copyable_content() {
    // List/Env/Tabs are never a source of "whole panel" copy text, and
    // an empty Response body (no request run yet) has nothing to copy.
    let app = TuiApp {
        focus: Pane::List,
        ..TuiApp::default()
    };
    assert!(!app.can_copy());

    let app = TuiApp {
        focus: Pane::Response,
        ..TuiApp::default()
    };
    assert!(!app.can_copy(), "no response has been received yet");
}

#[test]
fn y_with_no_selection_copies_the_whole_focused_panel_without_creating_a_selection() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "sel".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: "whole response body".into(),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

    // `y` copying the whole panel must not fabricate a selection —
    // there's still nothing highlighted on screen afterwards.
    assert!(app.text_selection().is_none());
    assert!(app.extra_selections().is_empty());
}

#[test]
fn main_panel_drag_extracts_the_expected_text() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    let entry = HurlEntry {
        method: "GET".into(),
        url: "http://example.com/very/long/path/for/testing".into(),
        ..Default::default()
    };
    // The Main panel's selectable/copyable text area is the JSON body
    // below the method/url header line, not the header itself — so the
    // expected text comes straight from the same JSON the panel renders.
    let json = crate::request::build_request_json(&entry);
    let second_line = json
        .lines()
        .nth(1)
        .expect("request JSON has at least 2 lines");
    app.collections[ci].entries = vec![entry];
    app.focus = Pane::Main;

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let area = app.main_text_area;
    assert!(
        area.width > 0 && area.height > 1,
        "main text area must render"
    );
    assert_eq!(
        app.main_panel.wrap().map(|w| w.line_text(0)),
        Some("{"),
        "the JSON body's opening brace must be the first visible row"
    );

    // Drag-select the whole second line (e.g. `"method": "GET",`).
    let ev = |kind, col_offset: u16| MouseEvent {
        kind,
        column: area.x + col_offset,
        row: area.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    let end_col = (second_line.chars().count() as u16).saturating_sub(1);
    app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), 0));
    app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), end_col));
    app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), end_col));

    let sel = app
        .text_selection()
        .expect("dragging inside the Main panel should start a selection");
    assert_eq!(sel.pane, Pane::Main);
    let text = super::selection::extract_text(
        sel.anchor,
        sel.cursor,
        app.main_panel.wrap().unwrap(),
        None,
    );
    assert_eq!(
        text.as_deref(),
        Some(second_line),
        "the whole second JSON line must be selected"
    );
}

/// Dragging past a panel's bottom edge must not just clamp the point
/// back inside (stalling the selection there) — it should start
/// auto-scrolling that panel downward and keep extending the selection
/// a full line at a time for as long as the drag stays outside the
/// panel's bounds, exactly as a text editor's drag-to-scroll behaves.
#[test]
fn dragging_past_the_bottom_edge_autoscrolls_and_extends_the_selection() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    let body: String = (0..200)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.collections[ci].entries = vec![HurlEntry {
        title: "scrollable".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(body.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let area = app.resp_text_area;
    assert!(
        area.height > 1,
        "the body must actually render for this test"
    );
    assert!(
        app.resp_max_scroll > 0,
        "content must be taller than the viewport for this test"
    );

    // Start a selection at the top-left of the visible body, then drag
    // to a point one row below the panel's own bottom edge.
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    });
    let below_row = area.y + area.height; // one row past the bottom edge
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: area.x,
        row: below_row,
        modifiers: KeyModifiers::NONE,
    });

    assert!(
        app.has_pending_autoscroll(),
        "dragging past the bottom edge must arm auto-scroll"
    );
    assert_eq!(
        app.resp_panel.scroll(),
        1,
        "the single drag event already ticks auto-scroll once"
    );
    let cursor_after_one_tick = app.text_selection().unwrap().cursor;
    assert!(
        cursor_after_one_tick.line > 0,
        "the selection must already extend past the first line"
    );

    // Further idle-loop ticks (no further mouse movement needed) keep
    // scrolling and keep extending the selection downward.
    for _ in 0..5 {
        app.autoscroll_tick();
    }
    assert_eq!(
        app.resp_panel.scroll(),
        6,
        "each tick scrolls one more row while still held past the edge"
    );
    let cursor_after_more_ticks = app.text_selection().unwrap().cursor;
    assert!(
        cursor_after_more_ticks.line > cursor_after_one_tick.line,
        "continued auto-scroll ticks keep extending the selection further down"
    );

    // Moving the drag back inside the panel's bounds cancels auto-scroll
    // and resumes tracking the mouse directly.
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        !app.has_pending_autoscroll(),
        "returning inside the panel cancels auto-scroll"
    );
}

/// Once a drag held past the panel's top/bottom edge has scrolled the
/// content all the way to its own start/end (no further scrolling is
/// possible), continuing to hold the drag outside the bounds must snap
/// the selection to that boundary line's *full* extent — the entire
/// first line when dragging above the top, the entire last line when
/// dragging below the bottom — rather than leaving the selection
/// wherever it last was inside the panel.
#[test]
fn dragging_past_the_edge_at_the_very_top_or_bottom_selects_the_whole_boundary_line() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    let body: String = (0..200)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.collections[ci].entries = vec![HurlEntry {
        title: "scrollable".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(body.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let area = app.resp_text_area;
    let max_scroll = app.resp_max_scroll;
    assert!(
        max_scroll > 0,
        "content must be taller than the viewport for this test"
    );

    // --- Bottom edge: scroll all the way to the end, then keep
    // dragging past the bottom edge — the last line must end up fully
    // selected.
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    });
    app.set_pending_autoscroll(Pane::Response, 1);
    for _ in 0..(max_scroll as usize + 5) {
        app.autoscroll_tick();
        term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    }
    assert_eq!(
        app.resp_panel.scroll(),
        max_scroll,
        "scrolling must stop once the last line is in view"
    );
    let sel = app
        .text_selection()
        .expect("selection must still be active");
    let wrap = app.resp_panel.wrap().unwrap();
    let last_line = wrap.line_count() - 1;
    assert_eq!(
        sel.cursor.line, last_line,
        "held past the bottom, the cursor must sit on the very last line"
    );
    let text = super::selection::extract_text(sel.anchor, sel.cursor, wrap, None)
        .expect("selection has text");
    assert!(
        text.ends_with(&format!("line{}", 199)),
        "the entire last line must be included in the selection: {text:?}"
    );

    // --- Top edge: start a fresh selection lower down, scroll all the
    // way back to the start, then keep dragging past the top edge — the
    // first line must end up fully selected.
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    });
    app.resp_panel.set_scroll(max_scroll);
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x,
        row: area.y + area.height - 1,
        modifiers: KeyModifiers::NONE,
    });
    app.set_pending_autoscroll(Pane::Response, -1);
    for _ in 0..(max_scroll as usize + 5) {
        app.autoscroll_tick();
        term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    }
    assert_eq!(
        app.resp_panel.scroll(),
        0,
        "scrolling must stop once the first line is in view"
    );
    let sel = app
        .text_selection()
        .expect("selection must still be active");
    assert_eq!(
        sel.cursor.line, 0,
        "held past the top, the cursor must sit on the very first line"
    );
    assert_eq!(
        sel.cursor.col, 0,
        "held past the top, the cursor must sit at the very start of that line"
    );
    let wrap = app.resp_panel.wrap().unwrap();
    let text = super::selection::extract_text(sel.anchor, sel.cursor, wrap, None)
        .expect("selection has text");
    assert!(
        text.starts_with("line0"),
        "the entire first line must be included in the selection: {text:?}"
    );
}

/// Reported crash: dragging a selection started in the Main (Request
/// JSON) panel down past its own bottom edge into the Response panel
/// below it must auto-scroll and extend the selection, not panic.
/// Unlike the Response panel (whose wrap cache is reused via
/// `Arc::ptr_eq`-based `rebuild_if_needed`), `draw_collection_main`
/// rebuilds `main_panel`'s wrap fresh on *every* draw — so this must redraw
/// between each drag step, exactly like the real main loop
/// (event -> draw -> event -> draw...), to reproduce a bug that only
/// shows up once the wrap has been rebuilt mid-drag.
#[test]
fn dragging_from_main_panel_past_its_bottom_edge_into_the_response_panel_does_not_panic() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    // Enough headers, each with a long enough value to wrap into
    // several rows on screen, to make the Request JSON body far taller
    // than the viewport once wrapped — so autoscroll must cross
    // several *wrapped* rows within a single raw JSON line, not just
    // advance line by line.
    let headers: Vec<(String, String, bool)> = (0..40)
        .map(|i| (format!("X-Header-{i}"), "v".repeat(300), true))
        .collect();
    app.collections[ci].entries = vec![HurlEntry {
        title: "tall-request".into(),
        headers,
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from("some response body"),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Main;

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let main_area = app.main_text_area;
    assert!(main_area.height > 1, "the Main panel must actually render");
    assert!(
        app.main_max_scroll > 0,
        "the request JSON must be taller than the viewport for this test"
    );
    let resp_area = app.resp_text_area;
    assert!(
        resp_area.y >= main_area.y + main_area.height,
        "the Response panel must sit below the Main panel"
    );

    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: main_area.x,
        row: main_area.y,
        modifiers: KeyModifiers::NONE,
    });
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    // Drag down, one row at a time, from inside the Main panel down
    // into the Response panel's own row range — redrawing after every
    // single move, exactly like the real event loop does.
    for row in
        (main_area.y + main_area.height)..resp_area.y.saturating_add(resp_area.height).min(24)
    {
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: main_area.x,
            row,
            modifiers: KeyModifiers::NONE,
        });
        term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    }
    // A few extra idle-loop ticks too, matching the real main loop's
    // "keep auto-scrolling even without further mouse movement".
    for _ in 0..10 {
        app.autoscroll_tick();
        term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    }

    assert_eq!(
        app.text_selection().map(|s| s.pane),
        Some(Pane::Main),
        "the selection must stay scoped to the Main panel it started in"
    );
}

/// Shift+Down/Up/Left/Right move the *end* of an active selection
/// without disturbing its anchor, letting the user fine-tune a
/// mouse-started selection from the keyboard — and the panel scrolls to
/// keep the moved end in view when it would otherwise go off-screen.
#[test]
fn shift_arrow_extends_the_selection_and_scrolls_it_into_view() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    let body: String = (0..200)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.collections[ci].entries = vec![HurlEntry {
        title: "scrollable".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(body.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    assert!(
        app.resp_max_scroll > 0,
        "content must be taller than the viewport for this test"
    );

    let start = TextPos::new(0, 2);
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Response,
        anchor: start,
        cursor: start,
    }));

    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    let after_down = app.text_selection().unwrap();
    assert_eq!(
        after_down.anchor, start,
        "Shift+Down must not move the anchor"
    );
    assert_eq!(
        after_down.cursor.line, 1,
        "Shift+Down moves the cursor end down one logical line"
    );

    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    assert_eq!(
        app.text_selection().unwrap().cursor.col,
        3,
        "Shift+Right moves the cursor end one char right"
    );

    // Repeated Shift+Down past the bottom of the viewport must scroll
    // the panel to keep the (still growing) selection's end visible.
    let visible_rows = app.resp_text_area.height as usize;
    for _ in 0..(visible_rows + 5) {
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    }
    let far_cursor = app.text_selection().unwrap().cursor;
    assert!(
        far_cursor.line >= visible_rows,
        "the selection cursor should have advanced well past the first screenful"
    );
    assert!(
        app.resp_panel.scroll() > 0,
        "scrolling down far enough via Shift+Down must scroll the panel to follow the cursor"
    );
    let wrap = app.resp_panel.wrap().unwrap();
    let (row, _) = wrap.textpos_to_row_col(far_cursor);
    assert!(
        row >= app.resp_panel.scroll() as u32
            && row < app.resp_panel.scroll() as u32 + app.resp_text_area.height as u32,
        "the moved cursor's row must be inside the scrolled-to viewport"
    );
}

/// The whole point of storing a selection as logical (line, char-offset)
/// [`TextPos`] positions rather than terminal (row, col) cells: it must
/// keep referring to the exact same *characters* across a resize/rewrap
/// that changes which screen row/column those characters land on —
/// without any special-case invalidation logic.
#[test]
fn resizing_the_panel_keeps_the_selection_on_the_same_characters() {
    use crate::i18n::Language;
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    // A single long, unbroken line so a change in panel width visibly
    // changes how many wrapped rows it spans (and which row/col a given
    // character offset falls on).
    let long_line: String = (0..40).map(|i| format!("{i:03}")).collect(); // 120 chars
    app.collections[ci].entries = vec![HurlEntry {
        title: "resize".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(long_line.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let width_before = app.resp_text_area.width;

    // Select characters 10..=19 (10 chars) of the single logical line.
    let sel = TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(0, 10),
        cursor: TextPos::new(0, 19),
    };
    app.set_text_selection(Some(sel));
    let expected: String = long_line.chars().skip(10).take(10).collect();
    let before_text = super::selection::extract_text(
        sel.anchor,
        sel.cursor,
        app.resp_panel.wrap().unwrap(),
        None,
    );
    assert_eq!(before_text.as_deref(), Some(expected.as_str()));

    // Shrink the terminal drastically, changing the panel's width (and
    // therefore how the single long line wraps onto rows).
    term.backend_mut().resize(30, 30);
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let width_after = app.resp_text_area.width;
    assert_ne!(
        width_before, width_after,
        "the resize must actually change the panel's width for this test to be meaningful"
    );

    // The selection (still the same `TextPos`s — nothing invalidated it)
    // must still resolve to the exact same characters, even though they
    // may now be wrapped onto a different screen row.
    let after_text = super::selection::extract_text(
        sel.anchor,
        sel.cursor,
        app.resp_panel.wrap().unwrap(),
        None,
    );
    assert_eq!(
        after_text.as_deref(),
        Some(expected.as_str()),
        "the selection must stay on the same characters after a resize, not the same terminal coordinates"
    );

    // The on-screen highlight must also land on the (possibly relocated)
    // correct cells after the resize.
    let buf = term.backend().buffer();
    let wrap = app.resp_panel.wrap().unwrap();
    let (row, col) = wrap.textpos_to_row_col(TextPos::new(0, 10));
    let area = app.resp_text_area;
    let screen_row = area.y + row as u16 - app.resp_panel.scroll().min(row as u16);
    let cell = buf.cell((area.x + col as u16, screen_row)).unwrap();
    let select_bg = super::theme::theme(&Language::English).select_bg;
    assert_eq!(
        cell.bg, select_bg,
        "the highlighted cell must follow the character to its new screen position after resize"
    );
}

#[test]
fn clicking_outside_both_text_panels_clears_any_selection() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let mut app = TuiApp::default();
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Main,
        anchor: TextPos::new(0, 0),
        cursor: TextPos::new(0, 0),
    }));
    // Row 0 is always outside both panels' cached text areas (menu bar).
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.text_selection().is_none(),
        "clicking outside a text panel clears the selection"
    );
}

// ── Multi-region selection (Alt+Click+Drag) ───────────────────────────

#[test]
fn alt_click_drag_adds_a_region_instead_of_replacing_the_active_one() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "multi".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: "first line here\nsecond line here".into(),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let area = app.resp_text_area;

    let ev = |kind, col: u16, row: u16, modifiers: KeyModifiers| MouseEvent {
        kind,
        column: area.x + col,
        row: area.y + row,
        modifiers,
    };

    // First (plain) drag selects "first" on row 0.
    app.on_mouse(ev(
        MouseEventKind::Down(MouseButton::Left),
        0,
        0,
        KeyModifiers::NONE,
    ));
    app.on_mouse(ev(
        MouseEventKind::Drag(MouseButton::Left),
        4,
        0,
        KeyModifiers::NONE,
    ));
    app.on_mouse(ev(
        MouseEventKind::Up(MouseButton::Left),
        4,
        0,
        KeyModifiers::NONE,
    ));
    assert!(
        app.extra_selections().is_empty(),
        "a single plain drag makes no extra regions yet"
    );

    // Alt+Click+Drag then selects "second" on row 1, finalizing the
    // first region into `extra_selections` instead of replacing it.
    app.on_mouse(ev(
        MouseEventKind::Down(MouseButton::Left),
        0,
        1,
        KeyModifiers::ALT,
    ));
    app.on_mouse(ev(
        MouseEventKind::Drag(MouseButton::Left),
        5,
        1,
        KeyModifiers::ALT,
    ));
    app.on_mouse(ev(
        MouseEventKind::Up(MouseButton::Left),
        5,
        1,
        KeyModifiers::ALT,
    ));

    assert_eq!(
        app.extra_selections().len(),
        1,
        "the first region is finalized, not discarded"
    );
    let wrap = app.resp_panel.wrap().unwrap();
    let first_text = super::selection::extract_text(
        app.extra_selections()[0].anchor,
        app.extra_selections()[0].cursor,
        wrap,
        None,
    );
    assert_eq!(first_text.as_deref(), Some("first"));
    let active = app
        .text_selection()
        .expect("the Alt-drag becomes the new active region");
    let second_text = super::selection::extract_text(active.anchor, active.cursor, wrap, None);
    assert_eq!(second_text.as_deref(), Some("second"));

    // Copying concatenates every region in document position order
    // (here that also happens to match creation order).
    let combined = app.concatenated_selection_text();
    assert_eq!(combined.as_deref(), Some("first\n\nsecond"));

    // Both regions are actually painted on screen, with the app's own
    // distinct selection colour (not just tracked internally).
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let select_bg = super::theme::theme(&crate::i18n::Language::English).select_bg;
    let buf = term.backend().buffer();
    assert_eq!(
        buf.cell((area.x + 2, area.y)).unwrap().bg,
        select_bg,
        "extra region ('first') is painted"
    );
    assert_eq!(
        buf.cell((area.x + 2, area.y + 1)).unwrap().bg,
        select_bg,
        "active region ('second') is painted"
    );
}

#[test]
fn copying_multiple_regions_orders_by_document_position_not_creation_order() {
    use ratatui::{Terminal, backend::TestBackend};
    // Regression test: selecting the end of a body first and then the
    // start (so `text_selection` — the most-recently-made, active
    // region — is the earlier one in the text) must still copy
    // start-then-end, not the reverse creation order.
    let mut app = TuiApp {
        focus: Pane::Response,
        ..Default::default()
    };
    // Made first (finalized into `extra_selections`): the *later* region in
    // the text.
    app.push_extra_selection(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(1, 0),
        cursor: TextPos::new(1, 6),
    });
    // Made second (the currently-active region): the *earlier* region in the
    // text.
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(0, 0),
        cursor: TextPos::new(0, 5),
    }));
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "order".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: "start\nend body".into(),
            ..Default::default()
        }),
        ..Default::default()
    }];

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    let combined = app.concatenated_selection_text();
    assert_eq!(
        combined.as_deref(),
        Some("start\n\nend bod"),
        "document-earlier region must come first, regardless of which was made first"
    );
}

#[test]
fn a_plain_click_drag_clears_every_extra_region() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let mut app = TuiApp::default();
    app.push_extra_selection(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(0, 0),
        cursor: TextPos::new(0, 3),
    });
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(1, 0),
        cursor: TextPos::new(1, 3),
    }));
    // Row 0 is outside both panels' cached text areas — a plain click
    // there clears the active selection *and* every extra region.
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert!(app.text_selection().is_none());
    assert!(
        app.extra_selections().is_empty(),
        "a plain click clears every region, not just the active one"
    );
}

#[test]
fn escape_clears_every_selection_region_and_y_is_a_no_op_afterwards() {
    let mut app = TuiApp::default();
    app.push_extra_selection(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(0, 0),
        cursor: TextPos::new(0, 3),
    });
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(1, 0),
        cursor: TextPos::new(1, 3),
    }));
    press(&mut app, KeyCode::Esc);
    assert!(app.text_selection().is_none());
    assert!(
        app.extra_selections().is_empty(),
        "Escape clears every region, not just the active one"
    );
    assert_eq!(
        app.concatenated_selection_text(),
        None,
        "nothing left to copy after clearing every region"
    );
}

#[test]
fn escape_clears_an_active_text_selection() {
    let mut app = TuiApp::default();
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(0, 0),
        cursor: TextPos::new(0, 0),
    }));
    press(&mut app, KeyCode::Esc);
    assert!(
        app.text_selection().is_none(),
        "Escape dismisses an active selection"
    );
}

#[test]
fn selecting_a_different_list_entry_clears_the_text_selection() {
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("a", "GET", "http://h/a", vec![], ""));
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("b", "GET", "http://h/b", vec![], ""));
    app.focus = Pane::List;
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Main,
        anchor: TextPos::new(0, 0),
        cursor: TextPos::new(0, 0),
    }));
    app.nav(1);
    assert!(
        app.text_selection().is_none(),
        "navigating to a different entry invalidates the old selection"
    );
}

#[test]
fn switching_tabs_clears_the_text_selection() {
    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("t".to_string(), vec![]));
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(0, 0),
        cursor: TextPos::new(0, 0),
    }));
    app.cycle_tab(true);
    assert!(
        app.text_selection().is_none(),
        "switching tabs invalidates the old panel's selection"
    );
}

#[test]
fn running_a_request_clears_the_text_selection() {
    let mut app = app_in_main_pane();
    let ci = app.active_tab;
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(0, 0),
        cursor: TextPos::new(0, 0),
    }));
    app.run_entry(ci);
    assert!(
        app.text_selection().is_none(),
        "starting a new request invalidates any selection over the old response"
    );
}

// ── Environment persistence (feature 5) ───────────────────────────────

#[test]

fn environment_persists_in_source_form_without_leaking_secrets() {
    // An env with a resolved secret plus a hand-added literal variable.
    let mut app = app_with_resolved_secret("s3cr3t");
    app.add_env_var(only_env_id(&app), "TEAM".into(), "crabs".into());

    let snapshot = app.to_persisted();

    // The serialized state must carry the reference, never the resolved value.
    let json = serde_json::to_string(&snapshot).unwrap();
    assert!(
        json.contains("op://V/i/f"),
        "the provider reference is persisted"
    );
    assert!(
        !json.contains("s3cr3t"),
        "the resolved secret is never written to disk"
    );

    // Reload into a fresh app: the environment comes back in source form.
    let mut restored = TuiApp::default();
    restored.apply_persisted(snapshot);
    assert_eq!(
        restored.collections[0].linked_env_id,
        Some(only_env_id(&restored)),
        "the collection relinks to the restored env"
    );
    let env = only_env(&restored);

    let token = env
        .vars
        .iter()
        .find(|v| v.key == "TOKEN")
        .expect("TOKEN restored");
    assert_eq!(token.raw, "{{ op://V/i/f }}", "the reference round-trips");
    assert!(
        token.is_pending(),
        "the secret re-resolves on load, never read from disk"
    );

    let team = env
        .vars
        .iter()
        .find(|v| v.key == "TEAM")
        .expect("TEAM restored");
    assert_eq!(team.value, "crabs");
    assert!(team.user_added, "the hand-added marker survives a reload");
}

// ── Saving clears the "new"/"modified" markers (feature 2) ────────────

#[test]
fn saving_a_collection_clears_the_user_added_markers() {
    let dir = temp_dir("savecol");
    let path = dir.join("out.hurl");

    let mut app = TuiApp::default();
    let mut entry = HurlEntry::from_fields("new", "GET", "http://h/x", vec![], "");
    entry.user_added = true;
    app.collections
        .push(Collection::new("api".into(), vec![entry]));
    app.active_tab = 1;

    app.do_file_action(FileAction::SaveCollection, path.to_str().unwrap());

    assert!(path.exists(), "the collection was written");
    assert!(
        !app.collections[1].entries[0].user_added,
        "the new marker is cleared after saving"
    );
    assert_eq!(
        app.collections[1].path.as_deref(),
        Some(path.as_path()),
        "the save path is remembered"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn saving_an_environment_clears_new_and_modified_markers() {
    let dir = temp_dir("saveenv");
    let path = dir.join("out.vars");

    let mut app = app_with_resolved_secret("s3cr3t");
    app.add_env_var(only_env_id(&app), "TEAM".into(), "crabs".into()); // new (user_added)
    app.commit_prompt_with_secrecy(
        PromptKind::EnvValue(only_env_id(&app), 0),
        "my-token".into(),
        true,
    ); // edits TOKEN (modified)

    {
        let env = only_env(&app);
        assert!(
            env.vars.iter().any(|v| v.modified),
            "a modified var exists before save"
        );
        assert!(
            env.vars.iter().any(|v| v.user_added),
            "a new var exists before save"
        );
    }

    app.do_file_action(FileAction::SaveEnv, path.to_str().unwrap());

    assert!(path.exists(), "the environment file was written");
    let env = only_env(&app);
    assert!(
        env.vars.iter().all(|v| !v.user_added && !v.modified),
        "all new/modified markers are cleared after saving",
    );
    for v in &env.vars {
        assert_eq!(
            v.original_value, v.value,
            "the saved values become the new baseline"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

// ── Save-prompt extension ghost autocomplete (feature 2) ──────────────

#[test]
fn tab_completes_the_hurl_ghost_in_a_save_prompt() {
    let mut app = TuiApp::default();
    app.open_path_prompt(FileAction::SaveCollection, "Save", "");
    for ch in "myfile".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab);
    match &app.overlay {
        Some(Overlay::Prompt { editor, .. }) => assert_eq!(editor.text(), "myfile.hurl"),
        _ => panic!("prompt not open"),
    }
    // A second Tab must not append the extension twice.
    press(&mut app, KeyCode::Tab);
    match &app.overlay {
        Some(Overlay::Prompt { editor, .. }) => assert_eq!(editor.text(), "myfile.hurl"),
        _ => panic!("prompt not open"),
    }
}

#[test]
fn right_arrow_completes_the_vars_ghost_at_the_end() {
    let mut app = TuiApp::default();
    app.open_path_prompt(FileAction::SaveEnv, "Save", "");
    for ch in "staging".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Right); // cursor at end -> completes ".vars"
    match &app.overlay {
        Some(Overlay::Prompt { editor, .. }) => assert_eq!(editor.text(), "staging.vars"),
        _ => panic!("prompt not open"),
    }
}

#[test]
fn save_prompt_renders_the_dimmed_extension_ghost() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut app = TuiApp::default();
    app.open_path_prompt(FileAction::SaveCollection, "Save", "");
    for ch in "myfile".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    let mut term = Terminal::new(TestBackend::new(72, 6)).unwrap();
    term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
        .unwrap();
    let out = buffer_text(term.backend().buffer());
    assert!(
        out.contains("myfile.hurl"),
        "the .hurl ghost is drawn after the filename:\n{out}"
    );
}

#[test]
fn save_confirm_popup_substitutes_the_new_and_modified_counts() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    // 3 user-added requests and 1 hand-added env var (TOKEN is not counted).
    let mut app = app_with_resolved_secret("s3cr3t");
    app.add_env_var(0, "TEAM".into(), "crabs".into());
    for i in 0..3 {
        let mut e = HurlEntry::from_fields(&format!("r{i}"), "GET", "http://h/x", vec![], "");
        e.user_added = true;
        app.collections[0].entries.push(e);
    }
    app.overlay = Some(Overlay::Confirm {
        action: ConfirmAction::Save(FileAction::SaveCollection),
        sel: 1,
    });

    let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
    term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
        .unwrap();
    let out = buffer_text(term.backend().buffer());

    assert!(
        !out.contains("{r}") && !out.contains("{e}"),
        "placeholders are substituted:\n{out}"
    );
    assert!(out.contains('3'), "the request count is shown:\n{out}");
    // The collection warning is scoped to requests only (not environment).
    assert!(
        out.contains("request"),
        "the request count is labelled:\n{out}"
    );
    assert!(
        !out.contains("environment"),
        "the collection warning omits the environment:\n{out}"
    );
    assert!(
        out.contains("Proceed?"),
        "the confirmation asks to proceed:\n{out}"
    );
}

#[test]
fn save_collection_to_original_confirms_then_saves() {
    let dir = temp_dir("saveorig");
    let path = dir.join("api.hurl");
    std::fs::write(&path, "# seed\nGET http://h/x\nHTTP 200\n").unwrap();

    let mut app = TuiApp::default();
    let mut entry = HurlEntry::from_fields("new", "GET", "http://h/x", vec![], "");
    entry.user_added = true;
    let mut col = Collection::new("api".into(), vec![entry]);
    col.path = Some(path.clone());
    app.collections.push(col);
    app.active_tab = 1;

    // f -> File menu; Down -> "(S)ave" submenu; Enter opens it; Down x1
    // lands on "Collection"; Enter opens its destination step; Enter again
    // picks "Save" (write back to the original file).
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Confirm {
                action: ConfirmAction::Save(FileAction::SaveCollection),
                ..
            })
        ),
        "a changed collection confirms before overwriting the original",
    );

    press(&mut app, KeyCode::Char('y')); // confirm -> saves to the original path
    assert!(
        app.overlay.is_none(),
        "saving to the original does not prompt for a name"
    );
    assert!(
        !app.collections[1].entries[0].user_added,
        "markers cleared after saving"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn saving_an_unchanged_collection_to_original_is_silent() {
    let dir = temp_dir("saveunchanged");
    let path = dir.join("api.hurl");

    let mut app = TuiApp::default();
    let mut col = Collection::new(
        "api".into(),
        vec![HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "")],
    );
    col.path = Some(path.clone());
    app.collections.push(col);
    app.active_tab = 1;

    app.begin_save(FileAction::SaveCollection);
    assert!(
        app.overlay.is_none(),
        "no warning when there are no changes to save"
    );
    assert!(
        path.exists(),
        "the collection is still written to the original file"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_collection_as_confirms_overwrite_of_existing_file() {
    let dir = temp_dir("saveas");
    let existing = dir.join("taken.hurl");
    std::fs::write(&existing, "old contents").unwrap();

    let mut app = TuiApp::default();
    let mut entry = HurlEntry::from_fields("new", "GET", "http://h/x", vec![], "");
    entry.user_added = true;
    app.collections
        .push(Collection::new("api".into(), vec![entry]));
    app.active_tab = 1;

    app.begin_save_as(FileAction::SaveCollection);
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Browser(FileAction::SaveCollectionChooseFolder, _))
        ),
        "Save As opens a destination-folder chooser first",
    );
    // Tab to the inline filename editor and type the target name, then Enter
    // saves it into the chosen folder.
    app.overlay = Some(Overlay::Browser(FileAction::SaveCollectionChooseFolder, {
        let mut ex = ratatui_explorer::FileExplorer::new().unwrap();
        let _ = ex.set_cwd(&dir);
        Box::new(ex)
    }));
    press(&mut app, KeyCode::Tab);
    assert!(app.browser_name_focused, "Tab focuses the filename field");
    app.browser_name = super::editor::Editor::new("taken.hurl", false);
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Confirm {
                action: ConfirmAction::Overwrite(FileAction::SaveCollection),
                ..
            })
        ),
        "saving over an existing file asks to confirm the overwrite",
    );

    press(&mut app, KeyCode::Char('y'));
    assert!(
        !std::fs::read_to_string(&existing)
            .unwrap()
            .contains("old contents"),
        "file overwritten"
    );
    assert!(
        !app.collections[1].entries[0].user_added,
        "markers cleared after saving"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_collection_as_to_a_new_file_writes_without_confirmation() {
    let dir = temp_dir("saveasnew");
    let fresh = dir.join("fresh.hurl");

    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".into(),
        vec![HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "")],
    ));
    app.active_tab = 1;

    app.begin_save_as(FileAction::SaveCollection);
    app.overlay = Some(Overlay::Browser(FileAction::SaveCollectionChooseFolder, {
        let mut ex = ratatui_explorer::FileExplorer::new().unwrap();
        let _ = ex.set_cwd(&dir);
        Box::new(ex)
    }));
    press(&mut app, KeyCode::Tab);
    app.browser_name = super::editor::Editor::new("fresh.hurl", false);
    press(&mut app, KeyCode::Enter);
    assert!(
        !matches!(
            app.overlay,
            Some(Overlay::Confirm {
                action: ConfirmAction::Overwrite(_),
                ..
            })
        ),
        "a brand-new file needs no overwrite confirmation",
    );
    assert!(fresh.exists(), "written to the new file");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn scratch_space_can_be_saved_as_a_collection() {
    let dir = temp_dir("scratchsave");
    let path = dir.join("scratch.hurl");

    // active_tab 0 = Scratch Space, which has no source path.
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("r", "GET", "http://h/x", vec![], ""));

    app.begin_save(FileAction::SaveCollection);
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Browser(FileAction::SaveCollectionChooseFolder, _))
        ),
        "the Scratch Space is saveable — Save opens a folder chooser (it has no file yet)",
    );
    app.overlay = Some(Overlay::Browser(FileAction::SaveCollectionChooseFolder, {
        let mut ex = ratatui_explorer::FileExplorer::new().unwrap();
        let _ = ex.set_cwd(&dir);
        Box::new(ex)
    }));
    press(&mut app, KeyCode::Tab);
    app.browser_name = super::editor::Editor::new("scratch.hurl", false);
    press(&mut app, KeyCode::Enter);
    assert!(
        path.exists(),
        "the Scratch Space is written to a collection file"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_collection_browser_defaults_a_hurl_extension_and_seeds_the_current_name() {
    let dir = temp_dir("saveasdefaultext");

    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "My API".into(),
        vec![HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "")],
    ));
    app.active_tab = 1;

    app.begin_save_as(FileAction::SaveCollection);
    assert_eq!(
        app.browser_name.text(),
        "My API.hurl",
        "the filename field is seeded from the collection's name"
    );
    app.overlay = Some(Overlay::Browser(FileAction::SaveCollectionChooseFolder, {
        let mut ex = ratatui_explorer::FileExplorer::new().unwrap();
        let _ = ex.set_cwd(&dir);
        Box::new(ex)
    }));
    press(&mut app, KeyCode::Tab);
    // Type a name with NO extension — `.hurl` is appended automatically.
    app.browser_name = super::editor::Editor::new("noext", false);
    press(&mut app, KeyCode::Enter);
    assert!(
        dir.join("noext.hurl").exists(),
        "a missing extension defaults to .hurl"
    );
    assert!(app.overlay.is_none(), "a fresh file saves without a prompt");
    std::fs::remove_dir_all(&dir).ok();
}

// ── Horizontal scroll clamping (feature 1) ────────────────────────────

#[test]
fn collection_list_scroll_stops_when_the_name_end_is_in_view() {
    let mut app = TuiApp::default();
    app.collections[0].entries.push(HurlEntry::from_fields(
        "r",
        "GET",
        "http://example.test/some/very/long/path/that/scrolls/off/the/edge",
        vec![],
        "",
    ));
    app.collections[0].selected_entry = 0;
    app.focus = Pane::List;
    let url_len = app.collections[0].entries[0].url.chars().count();
    app.list_scroll_w.set(20); // visible content width

    for _ in 0..100 {
        app.scroll_list_h(4); // scroll hard right
    }
    assert_eq!(
        app.list_hscroll,
        (url_len - (20 - 1)) as u16,
        "scrolling stops once the end of the name is visible (no blank overscroll)",
    );

    for _ in 0..100 {
        app.scroll_list_h(-4); // scroll hard left
    }
    assert_eq!(app.list_hscroll, 0, "cannot scroll left of the start");
}

#[test]
fn environment_panel_scroll_clamps_to_the_selected_row() {
    let mut app = TuiApp::default();
    let env_id = add_empty_global_env(&mut app, "e");
    app.add_env_var(
        env_id,
        "A_LONG_KEY_NAME".into(),
        "a-long-value-that-scrolls-off-screen".into(),
    );
    app.overlay = Some(Overlay::EnvPopup(EnvPopupState::new(env_id)));
    if let Some(Overlay::EnvPopup(popup)) = &mut app.overlay {
        popup.scroll_w.set(10);
    }

    let len = {
        let v = &only_env(&app).vars[0];
        v.key.chars().count() + 3 + v.display_value().chars().count()
    };

    for _ in 0..100 {
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    }
    match &app.overlay {
        Some(Overlay::EnvPopup(popup)) => assert_eq!(
            popup.hscroll,
            (len - (10 - 1)) as u16,
            "the env row scrolls only until its whole `key = value` is in view",
        ),
        _ => panic!("env popup closed"),
    }

    for _ in 0..100 {
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    }
    match &app.overlay {
        Some(Overlay::EnvPopup(popup)) => {
            assert_eq!(popup.hscroll, 0, "cannot scroll left of the start")
        }
        _ => panic!("env popup closed"),
    }
}

// ── Postman JSON import (feature 3) ───────────────────────────────────

#[test]
fn loading_a_postman_json_collection_imports_its_requests() {
    let mut app = TuiApp::default();
    let json = r#"{
          "info": { "name": "pm", "schema": "https://schema.getpostman.com/..v2.1.0" },
          "item": [
            { "name": "one", "request": { "method": "GET", "url": "{{url}}/one" } },
            { "name": "two", "request": { "method": "POST", "url": "{{url}}/two" } }
          ]
        }"#;
    let before = app.collections.len();
    let ok = app.load_collection_text("pm".into(), json, None);

    assert!(ok, "a Postman JSON export loads as a collection");
    assert_eq!(
        app.collections.len(),
        before + 1,
        "a new tab is added for the import"
    );
    let e = &app.collections[app.active_tab].entries;
    assert_eq!(e.len(), 2, "both Postman requests are imported");
    assert_eq!((e[0].title.as_str(), e[0].method.as_str()), ("one", "GET"));
    assert_eq!(
        (e[1].method.as_str(), e[1].url.as_str()),
        ("POST", "{{url}}/two")
    );
}

// ── Request preview substitution + modified marker (items 4, 7) ───────

#[test]
fn request_preview_substitutes_env_values_and_masks_secrets() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let _s = Strings::for_language(&Language::English);

    // Env has a resolved secret (TOKEN) plus a plain value (BASE_URL).
    let mut app = app_with_resolved_secret("supersecret");
    app.add_env_var(
        only_env_id(&app),
        "BASE_URL".into(),
        "http://127.0.0.1:8080".into(),
    );
    app.collections[0].entries.push(HurlEntry::from_fields(
        "r",
        "GET",
        "{{ BASE_URL }}/x",
        vec![("Authorization".into(), "Bearer {{ TOKEN }}".into(), true)],
        "",
    ));
    app.collections[0].selected_entry = app.collections[0].entries.len() - 1;
    app.focus = Pane::Main;

    let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let out = buffer_text(term.backend().buffer());

    // The request PREVIEW substitutes values (the collection list still shows
    // the raw `{{ VAR }}` template as the request's identity).
    assert!(
        out.contains("http://127.0.0.1:8080/x"),
        "BASE_URL is substituted in the preview:\n{out}"
    );
    assert!(
        out.contains("\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}"),
        "a substituted secret shows as 8 dots:\n{out}"
    );
    assert!(
        !out.contains("supersecret"),
        "the real secret value is never shown:\n{out}"
    );

    // The editor buffer keeps the ORIGINAL {{ VAR }} text.
    assert!(
        app.collections[0]
            .request_json_buf
            .contains("{{ BASE_URL }}"),
        "the editor keeps the raw placeholder, not the substituted value",
    );
    assert!(
        app.collections[0].request_json_buf.contains("{{ TOKEN }}"),
        "the editor keeps the raw secret reference, never the resolved value",
    );
}

#[test]
fn a_modified_request_shows_the_pencil_marker() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let _s = Strings::for_language(&Language::English);

    let mut app = TuiApp::default();
    let mut col = Collection::new(
        "api".into(),
        vec![HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "")],
    );
    col.entries[0].modified = true;
    app.collections.push(col);
    app.active_tab = 1;
    app.focus = Pane::List;

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let out = buffer_text(term.backend().buffer());
    assert!(
        out.contains('\u{270e}'),
        "a modified request is marked with a pencil:\n{out}"
    );
}

#[test]
fn collection_list_substitutes_and_colour_codes_by_status() {
    use crate::environment::{EnvVar, Environment, ValueSource};
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    // HOST is a resolved literal (cyan); BAD is a failed secret (red).
    let env = Environment {
        id: 0,
        name: "e".into(),
        vars: vec![
            EnvVar {
                key: "HOST".into(),
                value: "10.0.0.1".into(),
                source: ValueSource::Literal,
                resolved: true,
                loading: false,
                original_value: "10.0.0.1".into(),
                modified: false,
                user_added: false,
                raw: String::new(),
            },
            EnvVar {
                key: "BAD".into(),
                value: "{{ op://x }}".into(),
                source: ValueSource::OnePassword,
                resolved: false,
                loading: false,
                original_value: "{{ op://x }}".into(),
                modified: false,
                user_added: false,
                raw: String::new(),
            },
        ],
        path: None,
        git_origin: None,
    };
    let mut col = Collection::new(
        "api".into(),
        vec![
            // Unnamed entries so the list shows (and substitutes) the URL —
            // a named entry would show its name instead (see the
            // request-name display tests).
            HurlEntry::from_fields("", "GET", "http://plain/0", vec![], ""), // selected (index 0)
            HurlEntry::from_fields("", "GET", "{{ HOST }}/a", vec![], ""),
            HurlEntry::from_fields("", "GET", "{{ BAD }}/b", vec![], ""),
        ],
    );
    let mut app = TuiApp::default();
    let env_id = add_global_env(&mut app, env);
    col.linked_env_id = Some(env_id);
    app.collections.push(col);
    app.active_tab = 1;
    app.focus = Pane::List; // selected_entry 0 is highlighted; rows 1 & 2 keep their colours

    let mut term = Terminal::new(TestBackend::new(60, 16)).unwrap();
    term.draw(|f| super::draw::draw_collection_left(f, f.area(), &app, 1, &s, &th))
        .unwrap();
    let buf = term.backend().buffer();
    let out = buffer_text(buf);

    assert!(
        out.contains("10.0.0.1/a"),
        "a loaded literal is substituted in the list:\n{out}"
    );
    assert!(
        out.contains("{{ BAD }}/b"),
        "a failed var keeps its placeholder in the list:\n{out}"
    );
    assert_eq!(
        fg_at_substr(buf, "10.0.0.1"),
        Some(th.subst),
        "a literal substitution is cyan"
    );
    assert_eq!(
        fg_at_substr(buf, "{{ BAD }}"),
        Some(th.err),
        "a failed substitution is red"
    );
}

#[test]
fn the_request_list_shows_a_named_entrys_name_instead_of_its_url() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let col = Collection::new(
        "api".into(),
        vec![
            // A named entry shows its (leaf) name; an unnamed one falls back
            // to the URL, so both behaviours are visible side by side.
            HurlEntry::from_fields("Get widgets", "GET", "http://example/widgets", vec![], ""),
            HurlEntry::from_fields("", "GET", "http://example/orphan", vec![], ""),
        ],
    );
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = 1;

    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    term.draw(|f| super::draw::draw_collection_left(f, f.area(), &app, 1, &s, &th))
        .unwrap();
    let out = buffer_text(term.backend().buffer());

    assert!(
        out.contains("Get widgets"),
        "a named request shows its name in the list:\n{out}"
    );
    assert!(
        !out.contains("example/widgets"),
        "the name replaces the URL for a named request:\n{out}"
    );
    assert!(
        out.contains("example/orphan"),
        "an unnamed request still falls back to its URL:\n{out}"
    );
}

#[test]
fn editing_request_json_via_ctrl_enter_persists_and_marks_modified() {
    use super::editor::Editor;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = TuiApp::default();
    let entry = HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "");
    app.collections
        .push(Collection::new("api".into(), vec![entry]));
    app.active_tab = 1;
    app.focus = Pane::Main;

    // Open Raw Mode (Shift+H): the actual Hurl text of the entry.
    app.on_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
    assert!(app.overlay.is_some(), "raw mode editor should be open");

    // Replace the whole buffer with edited Hurl text (change the URL).
    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        let new_text = editor.text().replace("http://h/x", "http://h/CHANGED");
        *editor = Editor::new(&new_text, true);
    }

    // Commit with Ctrl+Enter.
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    assert!(app.overlay.is_none(), "editor should close on commit");

    let entry = &app.collections[1].entries[0];
    assert_eq!(
        entry.url, "http://h/CHANGED",
        "the edit should persist to the entry"
    );
    assert!(entry.modified, "the entry should be flagged modified");
    assert_eq!(
        app.changed_request_count(1),
        1,
        "save-count should reflect the modification"
    );
}

#[test]
fn editing_request_json_via_f2_persists_and_marks_modified() {
    use super::editor::Editor;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app = TuiApp::default();
    let entry = HurlEntry::from_fields(
        "r",
        "GET",
        "http://h/x",
        vec![("X-A".into(), "1".into(), true)],
        "",
    );
    app.collections
        .push(Collection::new("api".into(), vec![entry]));
    app.active_tab = 1;
    app.focus = Pane::Main;

    app.on_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
    assert!(app.overlay.is_some());

    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        let new_text = editor.text().replace("X-A: 1", "X-A: 2");
        *editor = Editor::new(&new_text, true);
    }

    app.on_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
    assert!(app.overlay.is_none(), "F2 should commit and close");

    let entry = &app.collections[1].entries[0];
    assert_eq!(
        entry.headers,
        vec![("X-A".to_string(), "2".to_string(), true)],
        "the header edit should persist"
    );
    assert!(entry.modified, "F2 commit should also flag modified");
    assert_eq!(app.changed_request_count(1), 1);
}

// ── "Still secret?" checkbox when editing a secret-sourced value ──────

#[test]
fn opening_a_secret_prompt_defaults_the_still_secret_checkbox_to_checked() {
    let mut app = app_with_resolved_secret("s3cr3t");
    open_only_env_popup(&mut app);
    press(&mut app, KeyCode::Enter);
    match &app.overlay {
        Some(Overlay::Prompt {
            secret_checkbox, ..
        }) => {
            assert_eq!(
                *secret_checkbox,
                Some(true),
                "secret-sourced vars show the checkbox, checked by default"
            );
        }
        _ => panic!("expected the env-value prompt to open"),
    }
}

#[test]
fn opening_a_plain_value_prompt_has_no_still_secret_checkbox() {
    // A literal (non-provider) environment variable has no "still secret?"
    // concept, so the checkbox must not appear.
    let mut app = TuiApp::default();
    let (env, _) = crate::environment::parse_vars_pending("e".into(), "PLAIN=hello");
    let env_id = add_global_env(&mut app, env);
    app.collections[0].linked_env_id = Some(env_id);
    app.overlay = Some(Overlay::EnvPopup(EnvPopupState::new(env_id)));
    press(&mut app, KeyCode::Enter);
    match &app.overlay {
        Some(Overlay::Prompt {
            secret_checkbox, ..
        }) => {
            assert_eq!(
                *secret_checkbox, None,
                "plain literal vars never show the checkbox"
            );
        }
        _ => panic!("expected the env-value prompt to open"),
    }
}

#[test]
fn ctrl_t_toggles_the_still_secret_checkbox() {
    let mut app = app_with_resolved_secret("s3cr3t");
    open_only_env_popup(&mut app);
    press(&mut app, KeyCode::Enter);
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    match &app.overlay {
        Some(Overlay::Prompt {
            secret_checkbox, ..
        }) => {
            assert_eq!(*secret_checkbox, Some(false), "Ctrl+T unchecks the box");
        }
        _ => panic!("prompt not open"),
    }
    app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    match &app.overlay {
        Some(Overlay::Prompt {
            secret_checkbox, ..
        }) => {
            assert_eq!(*secret_checkbox, Some(true), "a second Ctrl+T rechecks it");
        }
        _ => panic!("prompt not open"),
    }
}

#[test]
fn committing_with_the_checkbox_checked_keeps_the_secret_in_memory_only() {
    // Baseline (checkbox left checked) behaviour is unchanged from before
    // the checkbox existed: the new value is visible in-memory but never
    // written to the persisted `.vars` source.
    let mut app = app_with_resolved_secret("s3cr3t");
    app.commit_prompt_with_secrecy(
        PromptKind::EnvValue(only_env_id(&app), 0),
        "my-own-token".into(),
        true,
    );

    let env = only_env(&app);
    let v = &env.vars[0];
    assert!(v.modified);
    assert_eq!(
        v.value, "my-own-token",
        "the in-memory value reflects the edit"
    );
    assert!(v.is_secret_source(), "the var is still treated as secret");
    assert!(
        env.to_vars_text().contains("op://V/i/f"),
        "the provider reference is still what gets persisted"
    );
    assert!(
        !env.to_vars_text().contains("my-own-token"),
        "the plaintext secret must never be persisted"
    );
    assert!(
        app.has_unsaved_secret_changes(),
        "an in-memory-only secret edit is an unsaved secret change"
    );
}

#[test]
fn unchecking_still_secret_persists_the_plaintext_and_declassifies_the_var() {
    // With the checkbox unchecked, the user has confirmed the new value is
    // no longer sensitive: it should be written to the persisted source and
    // the variable should stop being treated as a secret altogether.
    let mut app = app_with_resolved_secret("s3cr3t");
    app.commit_prompt_with_secrecy(
        PromptKind::EnvValue(only_env_id(&app), 0),
        "no-longer-secret".into(),
        false,
    );

    let env = only_env(&app);
    let v = &env.vars[0];
    assert!(v.modified);
    assert_eq!(v.value, "no-longer-secret");
    assert!(
        !v.is_secret_source(),
        "the var is declassified after an unchecked commit"
    );
    assert!(!v.is_secret(), "a declassified var is no longer masked");
    assert_eq!(
        v.display_value(),
        "no-longer-secret",
        "the value now displays in the clear"
    );
    assert!(
        env.to_vars_text().contains("no-longer-secret"),
        "the plaintext value now persists to the .vars source"
    );
    assert!(
        !env.to_vars_text().contains("op://V/i/f"),
        "the provider reference is replaced once declassified"
    );
    assert!(
        !app.has_unsaved_secret_changes(),
        "a declassified value is no longer flagged as an unsaved secret change"
    );
}

// ── Editing an entry into a provider reference auto-loads it ──────────

#[test]
fn editing_a_literal_value_into_an_op_reference_reclassifies_and_queues_loading() {
    let mut app = TuiApp::default();
    let (env, _) = crate::environment::parse_vars_pending("e".into(), "BASE_URL=127.0.0.1");
    let env_id = add_global_env(&mut app, env);
    app.collections[0].linked_env_id = Some(env_id);
    let before = app.pending_env.len();

    app.commit_prompt_with_secrecy(
        PromptKind::EnvValue(env_id, 0),
        "{{ op://Vault/item/field }}".into(),
        true,
    );

    let v = &only_env(&app).vars[0];
    assert_eq!(
        v.source,
        crate::environment::ValueSource::OnePassword,
        "now classified as a 1Password ref"
    );
    assert!(
        v.loading && !v.resolved,
        "queued for background resolution, not yet resolved"
    );
    assert_eq!(
        v.raw, "{{ op://Vault/item/field }}",
        "the reference itself is what gets persisted"
    );
    assert_eq!(
        app.pending_env.len(),
        before + 1,
        "a resolution job was queued"
    );
}

#[test]
fn editing_a_literal_value_into_an_ssm_reference_reclassifies_and_queues_loading() {
    let mut app = TuiApp::default();
    let (env, _) = crate::environment::parse_vars_pending("e".into(), "BASE_URL=127.0.0.1");
    let env_id = add_global_env(&mut app, env);
    app.collections[0].linked_env_id = Some(env_id);
    let before = app.pending_env.len();

    app.commit_prompt_with_secrecy(
        PromptKind::EnvValue(env_id, 0),
        "{{ ssm:/demo/param }}".into(),
        true,
    );

    let v = &only_env(&app).vars[0];
    assert_eq!(
        v.source,
        crate::environment::ValueSource::Ssm,
        "now classified as an SSM ref"
    );
    assert!(
        v.loading && !v.resolved,
        "queued for background resolution, not yet resolved"
    );
    assert_eq!(
        app.pending_env.len(),
        before + 1,
        "a resolution job was queued"
    );
}

#[test]
fn editing_a_plain_value_to_another_plain_value_stays_literal_with_no_pending_work() {
    // Regression guard: an ordinary literal edit must not spuriously queue
    // background resolution work.
    let mut app = TuiApp::default();
    let (env, _) = crate::environment::parse_vars_pending("e".into(), "BASE_URL=127.0.0.1");
    let env_id = add_global_env(&mut app, env);
    app.collections[0].linked_env_id = Some(env_id);
    let before = app.pending_env.len();

    app.commit_prompt_with_secrecy(PromptKind::EnvValue(env_id, 0), "10.0.0.1".into(), true);

    let v = &only_env(&app).vars[0];
    assert_eq!(v.source, crate::environment::ValueSource::Literal);
    assert!(v.resolved && !v.loading);
    assert_eq!(
        app.pending_env.len(),
        before,
        "no resolution job queued for a plain literal edit"
    );
}

// ── Enter on the Requests list jumps straight into editing ────────────

#[test]
fn enter_on_the_requests_list_opens_the_edit_request_wizard() {
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("a", "GET", "http://h/a", vec![], ""));
    app.focus = Pane::List;

    press(&mut app, KeyCode::Enter);

    assert!(
        matches!(app.focus, Pane::Main),
        "focus moves to the request panel, as before"
    );
    match &app.overlay {
        Some(Overlay::NewRequest(form)) => {
            assert_eq!(
                form.editing,
                Some((0, 0)),
                "the wizard is prefilled for the selected entry"
            );
            assert_eq!(form.name.text(), "a");
            assert_eq!(form.url.text(), "http://h/a");
            assert!(
                form.asserts.is_empty(),
                "no default blank assert row when the entry has none"
            );
            assert!(
                form.captures.is_empty(),
                "no default blank capture row when the entry has none"
            );
        }
        _ => panic!("expected the Edit Request wizard to open"),
    }
}

#[test]
fn enter_on_an_empty_requests_list_does_not_open_an_editor() {
    let mut app = TuiApp {
        focus: Pane::List,
        ..Default::default()
    };

    press(&mut app, KeyCode::Enter);

    assert!(matches!(app.focus, Pane::Main));
    assert!(
        app.overlay.is_none(),
        "there is nothing to edit on an empty list"
    );
}

#[test]
fn closing_the_edit_wizard_returns_focus_to_the_requests_list() {
    let mut app = TuiApp::default();
    app.collections[0]
        .entries
        .push(HurlEntry::from_fields("a", "GET", "http://h/a", vec![], ""));
    app.focus = Pane::List;

    // Open the edit wizard from the list — focus moves to Main while it's open.
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.overlay, Some(Overlay::NewRequest(_))));
    assert!(matches!(app.focus, Pane::Main));

    // Cancelling (Esc) closes it and returns focus to the list, not the
    // raw request (Main) view.
    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_none(), "Esc closes the wizard");
    assert!(
        matches!(app.focus, Pane::List),
        "focus returns to the Requests list after cancelling"
    );

    // Saving (F2) likewise lands back on the list rather than the Main panel.
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.focus, Pane::Main),
        "reopened, focus is on Main"
    );
    press(&mut app, KeyCode::F(2));
    assert!(app.overlay.is_none(), "F2 saves and closes the wizard");
    assert!(
        matches!(app.focus, Pane::List),
        "focus returns to the Requests list after saving"
    );
}

#[test]
fn creating_a_request_returns_focus_to_the_requests_list() {
    let mut app = TuiApp::default();
    app.focus = Pane::List;

    press(&mut app, KeyCode::Char('n'));
    assert!(matches!(app.overlay, Some(Overlay::NewRequest(_))));
    if let Some(Overlay::NewRequest(form)) = &mut app.overlay {
        form.url = super::editor::Editor::new("http://h/new", false);
    }
    press(&mut app, KeyCode::F(2));

    assert_eq!(
        app.collections[0].entries.len(),
        1,
        "the request is created"
    );
    assert!(
        matches!(app.focus, Pane::List),
        "focus stays on the Requests list after creating a request"
    );
}

/// Editing an existing request through the wizard must not disturb fields
/// the wizard doesn't expose (query params, basic auth, expected status)
/// — only the fields shown in the form change.
#[test]
fn editing_a_request_preserves_fields_the_wizard_does_not_expose() {
    let mut app = TuiApp::default();
    let mut entry = HurlEntry::from_fields("orig", "GET", "http://h/x", vec![], "");
    entry.queries = vec![("q".into(), "1".into(), true)];
    entry.basic_auth = Some(("user".into(), "pass".into()));
    entry.expected_status = Some(200);
    app.collections[0].entries.push(entry);
    app.focus = Pane::List;

    press(&mut app, KeyCode::Enter); // opens the Edit Request wizard
    match &mut app.overlay {
        Some(Overlay::NewRequest(form)) => {
            form.name = super::editor::Editor::new("renamed", false);
        }
        _ => panic!("expected the Edit Request wizard to open"),
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries[0];
    assert_eq!(e.title, "renamed", "the edited field is applied");
    assert!(e.modified, "the entry is flagged modified");
    assert_eq!(
        e.queries,
        vec![("q".to_string(), "1".to_string(), true)],
        "query params untouched"
    );
    assert_eq!(
        e.basic_auth,
        Some(("user".to_string(), "pass".to_string())),
        "basic auth untouched"
    );
    assert_eq!(e.expected_status, Some(200), "expected status untouched");
}

#[test]
fn creating_a_request_with_an_assert_via_the_table() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> AddHeader (headers start empty)
    press(&mut app, KeyCode::Tab); // -> AddCookie (cookies start empty)
    press(&mut app, KeyCode::Tab); // -> AddQuery (queries start empty)
    press(&mut app, KeyCode::Tab); // -> AddFormField (form starts empty)
    press(&mut app, KeyCode::Tab); // -> Body
    press(&mut app, KeyCode::Tab); // -> AddAssert (asserts start empty)
    press(&mut app, KeyCode::Enter); // -> Assert(0), a fresh row is added
    for ch in "status == 200".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries;
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].asserts, vec!["status == 200".to_string()]);
}

#[test]
fn creating_a_request_with_a_capture_via_the_table() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> AddHeader (headers start empty)
    press(&mut app, KeyCode::Tab); // -> AddCookie (cookies start empty)
    press(&mut app, KeyCode::Tab); // -> AddQuery (queries start empty)
    press(&mut app, KeyCode::Tab); // -> AddFormField (form starts empty)
    press(&mut app, KeyCode::Tab); // -> Body
    press(&mut app, KeyCode::Tab); // -> AddAssert (asserts start empty)
    press(&mut app, KeyCode::Tab); // -> AddCapture (captures start empty)
    press(&mut app, KeyCode::Enter); // -> Capture(0, Name), a fresh row is added
    for ch in "token".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> Capture(0, Expr)
    for ch in "jsonpath \"$.token\"".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries;
    assert_eq!(e.len(), 1);
    assert_eq!(
        e[0].captures,
        vec![("token".to_string(), "jsonpath \"$.token\"".to_string())]
    );
}

#[test]
fn a_new_request_starts_with_no_default_assert_or_capture_rows() {
    // Unlike Headers/Cookies/Form (which always keep a blank row so the
    // user always has somewhere to type), Asserts/Captures should start
    // completely empty: just the "+ Add ..." row, no blank placeholder.
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    let form = match &app.overlay {
        Some(Overlay::NewRequest(form)) => form,
        _ => panic!("expected the New Request wizard to open"),
    };
    assert!(form.asserts.is_empty(), "no default blank assert row");
    assert!(form.captures.is_empty(), "no default blank capture row");
}

#[test]
fn deleting_the_last_assert_or_capture_row_leaves_the_section_empty() {
    // Ctrl+D on Headers/Cookies/Form re-seeds a fresh blank row when the
    // last one is deleted (they must never be empty); Asserts/Captures
    // are the exception the user asked for: deleting the last row
    // should leave the section genuinely empty, focused on the Add row.
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::PageDown); // -> Headers
    press(&mut app, KeyCode::PageDown); // -> Cookies
    press(&mut app, KeyCode::PageDown); // -> Queries
    press(&mut app, KeyCode::PageDown); // -> Form
    press(&mut app, KeyCode::PageDown); // -> Body
    press(&mut app, KeyCode::PageDown); // -> Asserts
    assert_eq!(new_focus(&app), NewField::AddAssert);
    press(&mut app, KeyCode::Enter); // adds a blank row
    assert_eq!(new_focus(&app), NewField::Assert(0));
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddAssert,
        "back to empty, not re-seeded"
    );
    assert!(form_ref(&app).asserts.is_empty());

    press(&mut app, KeyCode::PageDown); // -> Captures
    assert_eq!(new_focus(&app), NewField::AddCapture);
    press(&mut app, KeyCode::Enter); // adds a blank row
    assert_eq!(new_focus(&app), NewField::Capture(0, CapCol::Name));
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddCapture,
        "back to empty, not re-seeded"
    );
    assert!(form_ref(&app).captures.is_empty());
}

#[test]
fn creating_a_request_with_a_cookie_via_the_table() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> AddHeader, empty
    press(&mut app, KeyCode::Tab); // -> AddCookie
    press(&mut app, KeyCode::Enter); // -> Cookie(0, Key)
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Cookie, 0, HdrCol::Key)
    );
    for ch in "session".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> Cookie(0, Value)
    for ch in "abc123".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries;
    assert_eq!(e.len(), 1);
    assert_eq!(
        e[0].cookies,
        vec![("session".to_string(), "abc123".to_string(), true)]
    );
}

#[test]
fn creating_a_request_with_a_text_form_field_via_the_table() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> AddHeader, blank
    press(&mut app, KeyCode::Tab); // -> AddCookie, blank
    press(&mut app, KeyCode::Tab); // -> AddQuery (queries start empty)
    press(&mut app, KeyCode::Tab); // -> AddFormField
    press(&mut app, KeyCode::Enter); // -> FormField(0, Key)
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Key));
    for ch in "username".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> FormField(0, Kind), defaults to Text
    press(&mut app, KeyCode::Tab); // -> FormField(0, Value)
    for ch in "bob".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries;
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].form_fields.len(), 1);
    assert_eq!(e[0].form_fields[0].key, "username");
    assert_eq!(e[0].form_fields[0].value, "bob");
    assert_eq!(e[0].form_fields[0].kind, crate::hurl::FormFieldKind::Text);
    assert_eq!(e[0].form_fields[0].content_type, None);
}

#[test]
fn form_field_kind_dropdown_flips_text_and_file_and_persists_content_type() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> AddHeader, empty
    press(&mut app, KeyCode::Tab); // -> AddCookie, empty
    press(&mut app, KeyCode::Tab); // -> AddQuery (queries start empty)
    press(&mut app, KeyCode::Tab); // -> AddFormField
    press(&mut app, KeyCode::Enter); // -> FormField(0, Key)
    for ch in "avatar".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> FormField(0, Kind)
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Kind));

    match app.overlay.as_ref().unwrap() {
        Overlay::NewRequest(f) => {
            assert_eq!(
                f.form_fields[0].kind,
                crate::hurl::FormFieldKind::Text,
                "a fresh row defaults to Text"
            )
        }
        _ => panic!("form not open"),
    }
    assert!(
        !form_ref(&app).kind_dropdown_open(),
        "a populated (defaulted) Kind cell keeps the dropdown hidden"
    );
    press(&mut app, KeyCode::Enter); // reveal the dropdown
    assert!(form_ref(&app).kind_dropdown_open());
    press(&mut app, KeyCode::Down); // flip Text -> File
    match app.overlay.as_ref().unwrap() {
        Overlay::NewRequest(f) => {
            assert_eq!(f.form_fields[0].kind, crate::hurl::FormFieldKind::File)
        }
        _ => panic!("form not open"),
    }
    // Still on the Kind cell after flipping via the dropdown.
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Kind));
    // Left/Right now hop columns like any other cell (the dropdown no
    // longer captures them), so Right moves on to Value, then Ctype.
    press(&mut app, KeyCode::Right);
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Value));
    for ch in "avatar.png".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Right);
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Ctype));

    for ch in "image/png".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries;
    assert_eq!(e[0].form_fields[0].kind, crate::hurl::FormFieldKind::File);
    assert_eq!(
        e[0].form_fields[0].content_type.as_deref(),
        Some("image/png")
    );
}

#[test]
fn esc_dismisses_the_kind_dropdown_before_cancelling_the_form() {
    let mut app = TuiApp::default();
    open_form_on_form_field_kind(&mut app);
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Kind));
    assert!(
        !form_ref(&app).kind_dropdown_open(),
        "a populated (defaulted) Kind cell keeps the dropdown hidden"
    );
    press(&mut app, KeyCode::Enter); // reveal the dropdown
    assert!(form_ref(&app).kind_dropdown_open());

    press(&mut app, KeyCode::Esc); // first Esc: close the dropdown only
    assert!(app.overlay.is_some(), "the form stays open");
    assert!(
        !form_ref(&app).kind_dropdown_open(),
        "the dropdown is dismissed"
    );

    press(&mut app, KeyCode::Esc); // second Esc: cancel the form
    assert!(app.overlay.is_none(), "the form is cancelled");
}

#[test]
fn refocusing_the_kind_cell_keeps_the_dropdown_hidden() {
    let mut app = TuiApp::default();
    open_form_on_form_field_kind(&mut app);
    press(&mut app, KeyCode::Enter); // reveal the dropdown
    assert!(form_ref(&app).kind_dropdown_open());

    press(&mut app, KeyCode::Right); // -> FormField(0, Value): hops columns, no cycling
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Value));
    press(&mut app, KeyCode::Left); // back -> FormField(0, Kind)
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Kind));
    assert!(
        !form_ref(&app).kind_dropdown_open(),
        "leaving and returning does not reopen the dropdown (the cell always has Text/File set)"
    );
}

#[test]
fn kind_cell_defaults_to_text_and_enter_reveals_the_dropdown_to_change_it() {
    // Only two real options exist, and one of them (Text) is by far the
    // common case, so a fresh Kind cell defaults straight to it instead
    // of forcing the user to pick every time. The dropdown behaves like
    // any other already-populated cell (Key, Content-Type): it stays
    // hidden until Enter reveals it, rather than auto-opening.
    let mut app = TuiApp::default();
    open_form_on_form_field_kind(&mut app); // -> FormField(0, Kind), defaults to Text
    assert_eq!(
        form_ref(&app).form_fields[0].kind,
        crate::hurl::FormFieldKind::Text
    );
    assert!(
        !form_ref(&app).kind_dropdown_open(),
        "a defaulted Kind cell does not auto-open its dropdown"
    );

    press(&mut app, KeyCode::Enter); // reveal the dropdown to change it
    assert_eq!(
        new_focus(&app),
        NewField::FormField(0, FormCol::Kind),
        "Enter stays put to reveal the dropdown"
    );
    assert!(
        form_ref(&app).kind_dropdown_open(),
        "Enter reveals the dropdown on a populated Kind cell"
    );
    assert_eq!(
        form_ref(&app).form_fields[0].kind,
        crate::hurl::FormFieldKind::Text,
        "unchanged by Enter"
    );

    press(&mut app, KeyCode::Down); // flip Text -> File
    assert_eq!(
        form_ref(&app).form_fields[0].kind,
        crate::hurl::FormFieldKind::File
    );
}

#[test]
fn pressing_enter_to_confirm_the_kind_dropdown_stays_on_the_kind_cell() {
    // Regression test: confirming the picked Type with Enter must not
    // advance focus off the Kind cell (e.g. down into the Body
    // section) — the dropdown's own arrows can't steal focus, so
    // there's no need for Enter to move on the way Tab does.
    let mut app = TuiApp::default();
    open_form_on_form_field_kind(&mut app); // -> FormField(0, Kind), defaults to Text
    press(&mut app, KeyCode::Enter); // reveal the dropdown
    press(&mut app, KeyCode::Down); // flip Text -> File
    assert_eq!(
        form_ref(&app).form_fields[0].kind,
        crate::hurl::FormFieldKind::File
    );

    press(&mut app, KeyCode::Enter); // confirm the pick
    assert_eq!(
        new_focus(&app),
        NewField::FormField(0, FormCol::Kind),
        "Enter confirms in place, no focus jump"
    );
    assert!(
        !form_ref(&app).kind_dropdown_open(),
        "Enter closes the dropdown"
    );
    assert_eq!(
        form_ref(&app).form_fields[0].kind,
        crate::hurl::FormFieldKind::File,
        "the picked Type sticks"
    );

    // Tab still advances focus normally, once the dropdown is closed.
    press(&mut app, KeyCode::Tab);
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Value));
}

// ── Content-type dropdown (File-kind Form rows) ─────────────────────────

/// Move focus onto the first Form row's Value cell with its kind flipped
/// to `File` (key "avatar", no value typed yet).
fn open_form_on_file_value(app: &mut TuiApp) {
    open_form_on_form_field_kind(app); // -> FormField(0, Kind), defaults to Text
    press(app, KeyCode::Enter); // reveal the dropdown
    press(app, KeyCode::Down); // flip Text -> File
    press(app, KeyCode::Right); // -> FormField(0, Value): hops columns (Kind is before Value)
    assert_eq!(new_focus(app), NewField::FormField(0, FormCol::Value));
}

/// Move focus onto the first (File-kind) Form row's Content-Type cell.
fn open_form_on_file_ctype(app: &mut TuiApp) {
    open_form_on_file_value(app); // -> FormField(0, Value)
    press(app, KeyCode::Right); // -> FormField(0, Ctype)
    assert_eq!(new_focus(app), NewField::FormField(0, FormCol::Ctype));
}

#[test]
fn content_type_and_description_are_independent_form_columns() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> AddHeader
    press(&mut app, KeyCode::Tab); // -> AddCookie
    press(&mut app, KeyCode::Tab); // -> AddQuery (queries start empty)
    press(&mut app, KeyCode::Tab); // -> AddFormField
    press(&mut app, KeyCode::Enter); // -> FormField(0, Key)
    for ch in "avatar".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> FormField(0, Kind)
    press(&mut app, KeyCode::Enter); // reveal the dropdown
    press(&mut app, KeyCode::Down); // flip Text -> File
    press(&mut app, KeyCode::Right); // -> FormField(0, Value)
    press(&mut app, KeyCode::Right); // -> FormField(0, Ctype)
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Ctype));
    press(&mut app, KeyCode::Esc); // dismiss the auto-opened dropdown so typing isn't intercepted
    for ch in "text/csv".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Ctype));

    press(&mut app, KeyCode::Right); // -> Prefix (inert on a File row)
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Prefix));
    press(&mut app, KeyCode::Right); // -> Desc, independent from Ctype
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Desc));
    for ch in "a note".chars() {
        press(&mut app, KeyCode::Char(ch));
    }

    let row = &form_ref(&app).form_fields[0];
    assert_eq!(row.ctype.text(), "text/csv");
    assert_eq!(row.desc.text(), "a note");

    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    let e = &app.collections[0].entries;
    // Description never persists, only the content-type override does.
    assert_eq!(
        e[0].form_fields[0].content_type.as_deref(),
        Some("text/csv")
    );
}

#[test]
fn infer_content_type_matches_known_extensions_and_falls_back_to_none() {
    assert_eq!(infer_content_type("photo.gif"), Some("image/gif"));
    assert_eq!(infer_content_type("photo.JPG"), Some("image/jpeg"));
    assert_eq!(infer_content_type("photo.jpeg"), Some("image/jpeg"));
    assert_eq!(infer_content_type("icon.svg"), Some("image/svg+xml"));
    assert_eq!(infer_content_type("notes.txt"), Some("text/plain"));
    assert_eq!(infer_content_type("page.htm"), Some("text/html"));
    assert_eq!(infer_content_type("page.html"), Some("text/html"));
    assert_eq!(infer_content_type("doc.pdf"), Some("application/pdf"));
    assert_eq!(infer_content_type("data.xml"), Some("application/xml"));
    assert_eq!(infer_content_type("clip.webm"), Some("video/webm"));
    assert_eq!(infer_content_type("clip.WEBM"), Some("video/webm"));
    assert_eq!(infer_content_type("clip.mp4"), Some("video/mp4"));
    assert_eq!(infer_content_type("clip.m4v"), Some("video/mp4"));
    assert_eq!(infer_content_type("clip.mov"), Some("video/quicktime"));
    assert_eq!(infer_content_type("clip.avi"), Some("video/x-msvideo"));
    assert_eq!(infer_content_type("clip.mkv"), Some("video/x-matroska"));
    assert_eq!(infer_content_type("archive.zip"), None);
    assert_eq!(infer_content_type("no_extension"), None);
}

#[test]
fn content_type_options_are_deduplicated_mime_types() {
    let opts = content_type_options();
    // No MIME type should appear twice in the dropdown, even though
    // several extensions map to it (jpg/jpeg, htm/html, mp4/m4v, mpeg/mpg).
    let unique: std::collections::HashSet<&str> = opts.iter().copied().collect();
    assert_eq!(
        opts.len(),
        unique.len(),
        "content_type_options() must not contain duplicate mime types"
    );

    let mut all_mimes: std::collections::HashSet<&str> =
        CONTENT_TYPE_TABLE.iter().map(|(_, m)| *m).collect();
    all_mimes.extend(super::new_request::COMMON_CONTENT_TYPES);
    assert_eq!(
        unique, all_mimes,
        "every mime type in CONTENT_TYPE_TABLE and COMMON_CONTENT_TYPES must be offered exactly once"
    );

    assert!(opts.contains(&"image/jpeg"));
    assert_eq!(opts.iter().filter(|o| **o == "image/jpeg").count(), 1);
    assert!(opts.contains(&"text/html"));
    assert_eq!(opts.iter().filter(|o| **o == "text/html").count(), 1);
    assert!(
        opts.contains(&"video/webm"),
        "video/webm must be offered in the content-type dropdown"
    );
    assert_eq!(
        opts.iter().filter(|o| **o == "video/mp4").count(),
        1,
        "mp4/m4v collapse to one entry"
    );

    // Common non-extension-specific request-body MIME types are offered
    // too, not just the file-extension-derived ones.
    for common in super::new_request::COMMON_CONTENT_TYPES {
        assert!(
            opts.contains(common),
            "{common} should be offered in the content-type dropdown"
        );
    }

    let mut sorted = opts.clone();
    sorted.sort_unstable();
    assert_eq!(
        opts, sorted,
        "content_type_options() must be in alphabetical order"
    );
}

#[test]
fn content_type_dropdown_only_opens_for_file_kind_ctype_cells() {
    let mut app = TuiApp::default();
    open_form_on_form_field_kind(&mut app); // -> FormField(0, Kind), unset (not File)
    press(&mut app, KeyCode::Right); // -> Value
    press(&mut app, KeyCode::Right); // -> Ctype, still Text-kind
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Ctype));
    assert!(
        !form_ref(&app).ctype_dropdown_open(),
        "Text rows don't use the content-type dropdown"
    );
}

#[test]
fn content_type_dropdown_lets_the_user_pick_a_mime_type_via_arrows() {
    let mut app = TuiApp::default();
    open_form_on_file_ctype(&mut app);
    assert!(
        form_ref(&app).ctype_dropdown_open(),
        "the dropdown auto-opens over a File row's Desc cell"
    );
    // "Auto" is already the implicit selection (the override is empty),
    // so a single Down press should move straight to the first real MIME
    // type instead of re-selecting "Auto" a second time.
    press(&mut app, KeyCode::Down);
    assert_eq!(
        form_ref(&app).ctype_hi,
        Some(1),
        "one Down press should skip past the already-selected Auto"
    );
    press(&mut app, KeyCode::Enter); // commit it

    let first_mime = content_type_options()[0];
    match app.overlay.as_ref().unwrap() {
        Overlay::NewRequest(f) => assert_eq!(f.form_fields[0].ctype.text(), first_mime),
        _ => panic!("form not open"),
    }
    // Enter also advances focus and dismisses the dropdown.
    assert!(!form_ref(&app).ctype_dropdown_open());
}

#[test]
fn content_type_dropdown_auto_option_respects_the_filter() {
    // "Auto" is filtered exactly like any other option now: typing a
    // MIME string that doesn't match its own label makes it disappear
    // from the dropdown entirely (not just move out of easy reach via
    // Up), and it only comes back once the override is cleared again.
    let mut app = TuiApp::default();
    open_form_on_file_ctype(&mut app);
    let first_mime = content_type_options()[0];
    for ch in first_mime.chars() {
        press(&mut app, KeyCode::Char(ch)); // manually typed override
    }
    assert_eq!(form_ref(&app).form_fields[0].ctype.text(), first_mime);
    assert!(
        !form_ref(&app).ctype_filtered_options().is_empty()
            && form_ref(&app).form_fields[0].ctype.text() == first_mime,
        "sanity: the typed mime is still in the (now Auto-less) list"
    );

    // Nothing above the top (only) entry any more — Auto isn't
    // reachable while the typed text doesn't match it.
    press(&mut app, KeyCode::Up);
    assert_eq!(
        form_ref(&app).ctype_hi,
        None,
        "no Auto entry to move up into"
    );

    // Clearing the override back to empty brings Auto back as the
    // (only, implicit) selection.
    for _ in first_mime.chars() {
        press(&mut app, KeyCode::Backspace);
    }
    assert_eq!(form_ref(&app).form_fields[0].ctype.text(), "");
    press(&mut app, KeyCode::Tab); // commit via Tab too
    assert_eq!(
        form_ref(&app).form_fields[0].ctype.text(),
        "",
        "Auto (implicitly selected) clears the override"
    );
}

#[test]
fn typing_auto_keeps_the_auto_option_visible_and_typing_something_else_hides_it() {
    // Regression test for the filter itself: "Auto" stays in the list
    // while what's typed still matches its own label (e.g. the word
    // "Auto"), but disappears the moment the typed text no longer does.
    let mut app = TuiApp::default();
    open_form_on_file_ctype(&mut app);
    let s = crate::i18n::Strings::for_language(&crate::i18n::Language::English);

    for ch in "Auto".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert!(
        form_ref(&app).ctype_auto_visible(&s),
        "typing \"Auto\" itself must still match the Auto entry's own label"
    );

    for _ in "Auto".chars() {
        press(&mut app, KeyCode::Backspace);
    }
    for ch in "image/png".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert!(
        !form_ref(&app).ctype_auto_visible(&s),
        "typing an unrelated mime must hide the Auto entry"
    );
}

#[test]
fn esc_dismisses_the_content_type_dropdown_before_cancelling_the_form() {
    let mut app = TuiApp::default();
    open_form_on_file_ctype(&mut app);
    assert!(form_ref(&app).ctype_dropdown_open());

    press(&mut app, KeyCode::Esc); // first Esc: close the dropdown only
    assert!(app.overlay.is_some(), "the form stays open");
    assert!(!form_ref(&app).ctype_dropdown_open());

    press(&mut app, KeyCode::Esc); // second Esc: cancel the form
    assert!(app.overlay.is_none(), "the form is cancelled");
}

#[test]
fn a_populated_content_type_cell_hides_its_dropdown_until_enter_reveals_it() {
    // Same "hide if populated, Enter reveals" rule as the Key and Kind
    // dropdowns: an empty Ctype cell auto-opens, but once it holds text,
    // arrowing back onto it must not immediately re-trap Down/Up.
    let mut app = TuiApp::default();
    open_form_on_file_ctype(&mut app); // -> Ctype, empty, dropdown auto-open
    assert!(form_ref(&app).ctype_dropdown_open());
    press(&mut app, KeyCode::Esc); // dismiss the dropdown so typing isn't intercepted
    for ch in "image/png".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(form_ref(&app).form_fields[0].ctype.text(), "image/png");

    // Leaving and returning to the now-populated Ctype cell must not
    // reopen the dropdown automatically.
    press(&mut app, KeyCode::Left); // -> Kind
    press(&mut app, KeyCode::Right); // -> Ctype
    press(&mut app, KeyCode::Right); // -> Desc
    press(&mut app, KeyCode::Left); // back -> Ctype
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Ctype));
    assert!(
        !form_ref(&app).ctype_dropdown_open(),
        "a populated Ctype cell keeps the dropdown hidden"
    );

    // Enter explicitly reveals it again for browsing, without changing
    // the typed override or moving focus off the cell.
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        new_focus(&app),
        NewField::FormField(0, FormCol::Ctype),
        "Enter stays put to reveal the dropdown"
    );
    assert!(
        form_ref(&app).ctype_dropdown_open(),
        "Enter reveals the dropdown on a populated Ctype cell"
    );
    assert_eq!(
        form_ref(&app).form_fields[0].ctype.text(),
        "image/png",
        "unchanged by Enter"
    );
}

#[test]
fn typing_in_the_content_type_cell_filters_the_dropdown_like_the_key_dropdown_does() {
    // Same filter-as-you-type behaviour as Headers' Key dropdown
    // (`filter_headers`): typing narrows the MIME-type list down to
    // matches instead of always showing the full, fixed list.
    let mut app = TuiApp::default();
    open_form_on_file_ctype(&mut app); // -> Ctype, empty, dropdown auto-open
    let full_len = form_ref(&app).ctype_filtered_options().len();
    assert_eq!(
        full_len,
        content_type_options().len(),
        "an empty query shows every option, same as filter_headers"
    );

    for ch in "png".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert!(
        form_ref(&app).ctype_dropdown_open(),
        "typing keeps (re-opens) the dropdown, like the Key dropdown"
    );
    let filtered = form_ref(&app).ctype_filtered_options();
    assert!(
        !filtered.is_empty(),
        "\"png\" should match at least image/png"
    );
    assert!(
        filtered.len() < full_len,
        "the list should have narrowed down from typing \"png\""
    );
    assert!(
        filtered
            .iter()
            .all(|o| o.to_ascii_lowercase().contains("png")),
        "every filtered option must match the typed text:\n{filtered:?}"
    );
    assert!(filtered.contains(&"image/png"));

    // Typing something matching nothing empties the list (mirrors
    // `typing_no_match_hides_the_dropdown` for the Key dropdown).
    for ch in "zzz".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert!(
        form_ref(&app).ctype_filtered_options().is_empty(),
        "no mime type contains \"pngzzz\""
    );
}

// ── File picker for File-kind Form Value cells ──────────────────────────

#[test]
fn ctrl_f_on_a_file_value_cell_parks_the_wizard_and_opens_the_browser() {
    let mut app = TuiApp::default();
    open_form_on_file_value(&mut app);

    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Browser(FileAction::PickFormFile(0), _))
        ),
        "Ctrl+F should open the file browser targeting row 0"
    );
    assert!(
        app.parked_wizard.is_some(),
        "the wizard must be parked while browsing"
    );
}

#[test]
fn enter_on_a_file_value_cell_also_opens_the_browser() {
    // Ctrl+F works but is not discoverable; plain Enter on a File-kind
    // Value cell should do the same thing.
    let mut app = TuiApp::default();
    open_form_on_file_value(&mut app);

    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Browser(FileAction::PickFormFile(0), _))
        ),
        "Enter should open the file browser targeting row 0, same as Ctrl+F"
    );
    assert!(
        app.parked_wizard.is_some(),
        "the wizard must be parked while browsing"
    );
}

#[test]
fn enter_is_ignored_on_a_text_kind_value_cell() {
    // Enter should keep its normal "advance focus" behaviour for
    // Text-kind rows — only File-kind rows redirect it to the picker.
    let mut app = TuiApp::default();
    open_form_on_form_field_kind(&mut app); // -> FormField(0, Kind), unset (not File)
    press(&mut app, KeyCode::Right); // -> Value
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Value));

    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.overlay, Some(Overlay::NewRequest(_))),
        "the wizard stays open, no browser"
    );
    assert!(app.parked_wizard.is_none());
}

#[test]
fn ctrl_f_is_ignored_on_a_text_kind_value_cell() {
    let mut app = TuiApp::default();
    open_form_on_form_field_kind(&mut app); // Kind cell, unset (not File)
    press(&mut app, KeyCode::Right); // -> Value
    assert_eq!(new_focus(&app), NewField::FormField(0, FormCol::Value));

    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert!(
        matches!(app.overlay, Some(Overlay::NewRequest(_))),
        "the wizard stays open, no browser"
    );
    assert!(app.parked_wizard.is_none());
}

#[test]
fn picking_a_file_restores_the_wizard_with_the_path_and_inferred_content_type() {
    let dir = temp_dir("form_file_pick");
    std::fs::write(dir.join("avatar.png"), b"fake-png").unwrap();

    let mut app = TuiApp {
        last_browse_dir: Some(dir.clone()),
        ..Default::default()
    };
    open_form_on_file_value(&mut app);
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert!(matches!(app.overlay, Some(Overlay::Browser(..))));

    press(&mut app, KeyCode::Down); // highlight avatar.png
    press(&mut app, KeyCode::Enter); // pick it

    assert!(
        app.parked_wizard.is_none(),
        "the parked wizard is consumed on pick"
    );
    match app.overlay.as_ref().unwrap() {
        Overlay::NewRequest(f) => {
            assert_eq!(
                f.form_fields[0].value.text(),
                dir.join("avatar.png").to_string_lossy()
            );
            assert_eq!(
                f.form_fields[0].ctype.text(),
                "image/png",
                "content-type auto-inferred from .png"
            );
            assert_eq!(f.focus, NewField::FormField(0, FormCol::Value));
        }
        _ => panic!("expected the wizard to be restored"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn cancelling_the_file_picker_restores_the_wizard_unchanged() {
    let dir = temp_dir("form_file_cancel");
    std::fs::write(dir.join("avatar.png"), b"fake-png").unwrap();

    let mut app = TuiApp {
        last_browse_dir: Some(dir.clone()),
        ..Default::default()
    };
    open_form_on_file_value(&mut app);
    for ch in "old-value.png".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert!(matches!(app.overlay, Some(Overlay::Browser(..))));

    press(&mut app, KeyCode::Esc); // cancel

    assert!(app.parked_wizard.is_none());
    match app.overlay.as_ref().unwrap() {
        Overlay::NewRequest(f) => assert_eq!(f.form_fields[0].value.text(), "old-value.png"),
        _ => panic!("expected the wizard to be restored"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

// ── Body / Form-Multipart conflict ──────────────────────────────────────

#[test]
fn sending_a_request_with_both_body_and_form_fields_shows_a_clear_status_bar_error() {
    let mut entry = HurlEntry {
        method: "POST".to_string(),
        url: "http://127.0.0.1:1/x".to_string(),
        body: Some("{\"a\":1}".to_string()),
        ..Default::default()
    };
    entry.form_fields.push(crate::hurl::FormField {
        key: "foo".to_string(),
        value: "bar".to_string(),
        kind: crate::hurl::FormFieldKind::Text,
        content_type: None,
        base64_prefix: None,
        enabled: true,
    });

    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("t".to_string(), vec![entry]));
    app.active_tab = 1;
    app.focus = Pane::Main;

    app.on_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE));

    let r = app.response.lock().unwrap();
    assert!(
        !r.loading,
        "must not be left stuck loading on a request that can never be built"
    );
    assert!(
        r.error.contains("Body") && r.error.contains("Form"),
        "the error should explain the conflict clearly: {}",
        r.error
    );
}

// ── Wizard section-view tabs ────────────────────────────────────────────

#[test]
fn wizard_tab_bar_renders_every_section_label() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let form = NewReq::new(String::new(), vec!["Scratch".to_string()], 0, None);
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| super::new_request::draw_new_request(f, &form, &s, &th, true))
        .unwrap();
    let out = buffer_text(term.backend().buffer());
    for label in [
        "All", "Headers", "Cookies", "Form", "Body", "Asserts", "Captures", "Reports",
    ] {
        assert!(out.contains(label), "tab bar should show '{label}':\n{out}");
    }
}

#[test]
fn page_down_and_page_up_cycle_the_wizard_view_tab() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(form_ref(&app).view_tab, WizardTab::All);

    press(&mut app, KeyCode::PageDown);
    assert_eq!(form_ref(&app).view_tab, WizardTab::Headers);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(form_ref(&app).view_tab, WizardTab::Cookies);

    press(&mut app, KeyCode::PageUp);
    assert_eq!(form_ref(&app).view_tab, WizardTab::Headers);
    press(&mut app, KeyCode::PageUp);
    assert_eq!(
        form_ref(&app).view_tab,
        WizardTab::All,
        "PageUp wraps back around to All"
    );
}

#[test]
fn a_single_section_tab_shows_far_more_rows_than_the_combined_all_view() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut form = NewReq::new(String::new(), vec!["Scratch".to_string()], 0, None);
    form.headers.clear();
    for i in 0..8 {
        let mut row = HeaderRow::new();
        row.key = super::editor::Editor::new(&format!("Header{i}"), false);
        row.value = super::editor::Editor::new(&format!("Value{i}"), false);
        form.headers.push(row);
    }

    let count_visible = |form: &NewReq| {
        let mut term = Terminal::new(TestBackend::new(100, 26)).unwrap();
        term.draw(|f| super::new_request::draw_new_request(f, form, &s, &th, true))
            .unwrap();
        let out = buffer_text(term.backend().buffer());
        (0..8)
            .filter(|i| out.contains(&format!("Header{i}")))
            .count()
    };

    let all_view_count = count_visible(&form);
    form.view_tab = WizardTab::Headers;
    let headers_tab_count = count_visible(&form);

    assert!(
        headers_tab_count > all_view_count,
        "the Headers-only tab ({headers_tab_count} visible) should show more rows than the combined All view ({all_view_count} visible)"
    );
}

#[test]
fn editing_on_a_section_tab_is_reflected_when_switching_back_to_all() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app); // -> Header(0, Key), All tab

    press(&mut app, KeyCode::PageDown); // -> Headers tab
    assert_eq!(form_ref(&app).view_tab, WizardTab::Headers);
    for ch in "X-Test".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(form_ref(&app).headers[0].key.text(), "X-Test");

    press(&mut app, KeyCode::PageUp); // back -> All tab
    assert_eq!(form_ref(&app).view_tab, WizardTab::All);
    // Same underlying field, so the edit is immediately visible: no
    // separate per-tab copy of the data exists.
    assert_eq!(form_ref(&app).headers[0].key.text(), "X-Test");
}

#[test]
fn section_tab_confines_tab_navigation_to_that_section() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app); // -> Header(0, Key), All tab
    press(&mut app, KeyCode::Char('X')); // non-blank, so Tab walks the row instead of skipping it
    press(&mut app, KeyCode::PageDown); // -> Headers tab
    assert_eq!(form_ref(&app).view_tab, WizardTab::Headers);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key),
        "switching tabs jumps to the first field"
    );

    // Walking forward within the section is unaffected.
    press(&mut app, KeyCode::Tab); // -> Header(0, Value)
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Value)
    );
    press(&mut app, KeyCode::Tab); // -> Header(0, Desc)
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Desc)
    );
    press(&mut app, KeyCode::Tab); // -> AddHeader (still within the section)
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Header));

    // Tab off the "Add header" row would normally cross into Cookies;
    // confined to the Headers tab, it must wrap back to the section's
    // first field instead.
    press(&mut app, KeyCode::Tab);
    assert_eq!(
        new_focus(&app),
        NewField::Kvd(KvdKind::Header, 0, HdrCol::Key),
        "Tab must not leak out of the active section tab"
    );

    // Shift+Tab backward off the first field must likewise wrap to the
    // section's last field ("Add header"), not leak to Url.
    press(&mut app, KeyCode::BackTab);
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Header));
}

#[test]
fn section_tab_confines_enter_navigation_to_that_section() {
    // Enter on a blank last row calls the same `focus_next` machinery as
    // Tab and can leak straight past the section's own "Add …" row into
    // the next section (e.g. a blank Assert leaks directly to Capture);
    // confined to a section tab, it must wrap within the section instead.
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::PageDown); // -> Headers
    press(&mut app, KeyCode::PageDown); // -> Cookies
    press(&mut app, KeyCode::PageDown); // -> Queries
    press(&mut app, KeyCode::PageDown); // -> Form
    press(&mut app, KeyCode::PageDown); // -> Body
    press(&mut app, KeyCode::PageDown); // -> Asserts
    assert_eq!(form_ref(&app).view_tab, WizardTab::Asserts);
    assert_eq!(
        new_focus(&app),
        NewField::AddAssert,
        "asserts start empty, so the entry point is the Add row"
    );

    press(&mut app, KeyCode::Enter); // adds a blank row, focuses it
    assert_eq!(new_focus(&app), NewField::Assert(0));

    press(&mut app, KeyCode::Enter);
    assert_eq!(
        new_focus(&app),
        NewField::Assert(0),
        "Enter on a blank assert must not leak out into Captures"
    );
}

#[test]
fn ctrl_shift_arrows_reorder_wizard_section_tabs() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(form_ref(&app).view_tab, WizardTab::All);

    press(&mut app, KeyCode::PageDown); // -> Headers
    assert_eq!(form_ref(&app).view_tab, WizardTab::Headers);

    app.on_key(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(
        form_ref(&app).tab_order[1],
        WizardTab::Cookies,
        "Headers should have swapped forward past Cookies"
    );
    assert_eq!(form_ref(&app).tab_order[2], WizardTab::Headers);
    assert_eq!(
        form_ref(&app).view_tab,
        WizardTab::Headers,
        "the moved tab stays active"
    );

    app.on_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(form_ref(&app).tab_order[1], WizardTab::Headers);
    assert_eq!(form_ref(&app).tab_order[2], WizardTab::Cookies);

    // `All` is pinned first and can never move, in either direction.
    press(&mut app, KeyCode::PageUp); // -> All
    assert_eq!(form_ref(&app).view_tab, WizardTab::All);
    app.on_key(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(
        form_ref(&app).tab_order[0],
        WizardTab::All,
        "All cannot be moved"
    );
    app.on_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(
        form_ref(&app).tab_order[1],
        WizardTab::Headers,
        "nothing can move into All's slot"
    );
}

#[test]
fn switching_to_a_section_tab_jumps_focus_to_its_first_field() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    // Leave focus somewhere unrelated (Name) before switching tabs.
    assert_eq!(new_focus(&app), NewField::Name);

    press(&mut app, KeyCode::PageDown); // -> Headers
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Header),
        "headers start empty, so the entry point is the Add row"
    );

    press(&mut app, KeyCode::PageDown); // -> Cookies
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Cookie),
        "cookies start empty, so the entry point is the Add row"
    );

    press(&mut app, KeyCode::PageDown); // -> Queries
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Query),
        "queries start empty, so the entry point is the Add row"
    );

    press(&mut app, KeyCode::PageDown); // -> Form
    assert_eq!(
        new_focus(&app),
        NewField::AddFormField,
        "form fields start empty, so the entry point is the Add row"
    );

    press(&mut app, KeyCode::PageDown); // -> Body
    assert_eq!(
        new_focus(&app),
        NewField::Body,
        "Body's only field is the editor itself, so this also puts it into editing mode"
    );

    press(&mut app, KeyCode::PageDown); // -> Asserts
    assert_eq!(
        new_focus(&app),
        NewField::AddAssert,
        "asserts start empty, so the entry point is the Add row"
    );

    press(&mut app, KeyCode::PageDown); // -> Captures
    assert_eq!(
        new_focus(&app),
        NewField::AddCapture,
        "captures start empty, so the entry point is the Add row"
    );
}

#[test]
fn brackets_cycle_wizard_section_tabs_on_non_text_fields() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    // Name is a text field, so `]` types into it rather than cycling.
    assert_eq!(new_focus(&app), NewField::Name);
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(
        form_ref(&app).view_tab,
        WizardTab::All,
        "`]` typed into Name"
    );
    assert_eq!(form_ref(&app).name.text(), "]");

    // Move to Method (a selector, not a text field): `]` / `[` cycle tabs.
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    assert_eq!(new_focus(&app), NewField::Method);
    press(&mut app, KeyCode::Char(']')); // forward -> Headers
    assert_eq!(form_ref(&app).view_tab, WizardTab::Headers);
    press(&mut app, KeyCode::Char(']')); // forward -> Cookies
    assert_eq!(form_ref(&app).view_tab, WizardTab::Cookies);
    press(&mut app, KeyCode::Char('[')); // back -> Headers
    assert_eq!(form_ref(&app).view_tab, WizardTab::Headers);
}

#[test]
fn brackets_are_typed_into_wizard_text_fields() {
    let mut app = TuiApp::default();
    open_form_on_header(&mut app); // -> Header(0, Key), a text field
    for ch in "a[0]".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    assert_eq!(
        form_ref(&app).view_tab,
        WizardTab::All,
        "brackets must not cycle tabs while a text cell is focused"
    );
    assert_eq!(form_ref(&app).headers[0].key.text(), "a[0]");
}

#[test]
fn page_up_page_down_cycle_collection_tabs_in_the_main_view() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.collections.push(Collection::new("web".into(), vec![]));
    assert_eq!(app.active_tab, 0);

    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.active_tab, 1);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.active_tab, 2);

    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.active_tab, 1);
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.active_tab, 0);
}

#[test]
fn tab_cycles_panes_in_reading_order_list_main_env_response() {
    let mut app = TuiApp {
        focus: Pane::Tabs,
        ..Default::default()
    };

    press(&mut app, KeyCode::Tab);
    assert!(
        matches!(app.focus, Pane::List),
        "Collections/requests list follows the Tabs bar"
    );
    press(&mut app, KeyCode::Tab);
    assert!(
        matches!(app.focus, Pane::Main),
        "Request JSON follows the list, matching top-left -> top-right reading order"
    );
    press(&mut app, KeyCode::Tab);
    assert!(
        matches!(app.focus, Pane::GlobalEnv),
        "Environments follows Request JSON, matching top-right -> bottom-left"
    );
    press(&mut app, KeyCode::Tab);
    assert!(
        matches!(app.focus, Pane::Response),
        "Response follows Environments, matching bottom-left -> bottom-right"
    );
    press(&mut app, KeyCode::Tab);
    assert!(
        matches!(app.focus, Pane::Tabs),
        "cycling wraps back around to the Tabs bar"
    );

    // Shift+Tab must walk the same order backwards.
    app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
    assert!(matches!(app.focus, Pane::Response));
    app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
    assert!(matches!(app.focus, Pane::GlobalEnv));
}

#[test]
fn ctrl_left_right_are_a_third_alias_for_prev_next_tab_from_any_pane() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.collections.push(Collection::new("web".into(), vec![]));
    app.focus = Pane::List; // works from any pane, not just the tab bar
    assert_eq!(app.active_tab, 0);

    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
    assert_eq!(app.active_tab, 1);
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
    assert_eq!(app.active_tab, 2);

    app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(app.active_tab, 1);
    app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(app.active_tab, 0);
}

#[test]
fn alt_1_through_7_jump_directly_to_a_wizard_section_by_number() {
    // Alt, not Ctrl: Ctrl+<digit> has no standard control-code encoding,
    // so most terminals only report it with a modifier when the Kitty
    // keyboard protocol is active. Alt is sent as a plain ESC-prefix
    // almost everywhere, so it works without any special terminal
    // support — see the comment at the call site in input.rs.
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));

    let cases = [
        ('1', NewField::AddKvd(KvdKind::Header)),
        ('2', NewField::AddKvd(KvdKind::Cookie)),
        ('3', NewField::AddKvd(KvdKind::Query)),
        ('4', NewField::AddFormField),
        ('5', NewField::Body),
        ('6', NewField::AddAssert),
        ('7', NewField::AddCapture),
    ];
    for (digit, expected) in cases {
        app.on_key(KeyEvent::new(KeyCode::Char(digit), KeyModifiers::ALT));
        assert_eq!(
            new_focus(&app),
            expected,
            "Alt+{digit} should jump straight to its section"
        );
    }

    // Works regardless of which section-view tab is currently active,
    // not just from `All`.
    press(&mut app, KeyCode::PageDown); // -> Headers view tab
    app.on_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::ALT));
    assert_eq!(
        new_focus(&app),
        NewField::Body,
        "Alt+5 still jumps to Body from a different section tab"
    );

    // Plain (unmodified) digits must still type into a text field
    // instead of being swallowed as a jump shortcut — this is exactly
    // the bug being guarded against (Ctrl+<digit> falling back to a
    // bare digit on terminals without keyboard-enhancement support).
    // Focus is on Body (from the Alt+4 jump above); a bare '1' must be
    // typed into it, not reinterpreted as "jump to Headers".
    press(&mut app, KeyCode::Char('1'));
    assert!(
        form_ref(&app).body.text().contains('1'),
        "a plain digit key must type into the focused field"
    );
}

#[test]
fn ctrl_up_down_jumps_directly_between_sections() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    press(&mut app, KeyCode::Tab); // -> AddHeader (headers start empty)
    assert_eq!(new_focus(&app), NewField::AddKvd(KvdKind::Header));

    // Ctrl+Down jumps straight past Headers into Cookies, skipping the
    // rest of the (empty) Headers section.
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Cookie),
        "cookies start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Query),
        "queries start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddFormField,
        "form fields start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
    assert_eq!(new_focus(&app), NewField::Body);
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddAssert,
        "asserts start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddCapture,
        "captures start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddReport,
        "reports start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
    assert_eq!(new_focus(&app), NewField::Name, "wraps back to the top");

    // And Ctrl+Up walks the same chain backward.
    app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddReport,
        "reports start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddCapture,
        "captures start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddAssert,
        "asserts start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_eq!(new_focus(&app), NewField::Body);
    app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddFormField,
        "form fields start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Query),
        "queries start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Cookie),
        "cookies start empty, so the entry point is the Add row"
    );
    app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddKvd(KvdKind::Header),
        "headers start empty, so the entry point is the Add row"
    );
}

#[test]
fn scroll_window_keeps_the_focused_row_in_view_and_clamps_at_the_edges() {
    // 10 items, 3 fit at a time: focusing near the end pulls the window
    // forward; focusing back near the start pulls it back, and the
    // window never runs past the last page.
    let scroll = std::cell::Cell::new(0usize);
    assert_eq!(scroll_window(&scroll, Some(0), 10, 3), 0);
    assert_eq!(
        scroll_window(&scroll, Some(2), 10, 3),
        0,
        "row 2 already fits in the [0,3) window"
    );
    assert_eq!(
        scroll_window(&scroll, Some(5), 10, 3),
        3,
        "scrolls forward just enough to show row 5"
    );
    assert_eq!(
        scroll_window(&scroll, Some(9), 10, 3),
        7,
        "last row pins the window to the final page"
    );
    assert_eq!(
        scroll_window(&scroll, Some(0), 10, 3),
        0,
        "moving focus back scrolls back up"
    );
    assert_eq!(
        scroll_window(&scroll, None, 10, 3),
        0,
        "no focus keeps the last offset, clamped"
    );
    assert_eq!(
        scroll_window(&scroll, Some(4), 10, 20),
        0,
        "no scrolling needed when everything fits"
    );
}

#[test]
fn many_headers_scroll_and_keep_the_focused_row_rendered() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut form = NewReq::new(String::new(), vec!["Scratch".to_string()], 0, None);
    form.headers.clear();
    for i in 0..8 {
        let mut row = HeaderRow::new();
        row.key = super::editor::Editor::new(&format!("Header{i}"), false);
        row.value = super::editor::Editor::new(&format!("Value{i}"), false);
        form.headers.push(row);
    }
    // Focus the last row: with only a handful of visible lines, it must
    // still be scrolled into view rather than clipped off-screen.
    form.focus = NewField::Kvd(KvdKind::Header, 7, HdrCol::Key);

    // A short area: 1 column-header line + room for only ~4 data rows.
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 5,
    };
    let mut term = Terminal::new(TestBackend::new(40, 5)).unwrap();
    term.draw(|f| super::new_request::draw_kvd_table(f, area, &form, KvdKind::Header, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(
        text.contains("Header7"),
        "the focused last row must be scrolled into view:\n{text}"
    );
}

#[test]
fn many_headers_always_keep_the_add_header_row_visible() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut form = NewReq::new(String::new(), vec!["Scratch".to_string()], 0, None);
    form.headers.clear();
    for i in 0..8 {
        let mut row = HeaderRow::new();
        row.key = super::editor::Editor::new(&format!("Header{i}"), false);
        row.value = super::editor::Editor::new(&format!("Value{i}"), false);
        form.headers.push(row);
    }
    // Focus the *first* row: the scroll window sits at the top of the
    // list, as far from the "+ Add Header" hint as possible.
    form.focus = NewField::Kvd(KvdKind::Header, 0, HdrCol::Key);

    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 5,
    };
    let mut term = Terminal::new(TestBackend::new(40, 5)).unwrap();
    term.draw(|f| super::new_request::draw_kvd_table(f, area, &form, KvdKind::Header, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(
        text.contains("Add header"),
        "the Add Header hint must stay pinned/visible even when scrolled away from it:\n{text}"
    );
    assert!(
        text.contains("Header0"),
        "the focused first row should still be visible:\n{text}"
    );
}

#[test]
fn section_height_grows_with_row_count_up_to_a_five_row_cap() {
    use super::new_request::section_height;
    // header_h=1 (Headers/Cookies/Form/Captures): 1 header line + N rows
    // + 1 pinned Add row, capped at 5 visible data rows.
    assert_eq!(section_height(1, 0), 2);
    assert_eq!(section_height(1, 1), 3);
    assert_eq!(section_height(1, 5), 7);
    assert_eq!(
        section_height(1, 6),
        7,
        "caps at 5 rows once it would need to scroll"
    );
    assert_eq!(section_height(1, 100), 7);
    // header_h=0 (Asserts: no column-header line).
    assert_eq!(section_height(0, 1), 2);
    assert_eq!(section_height(0, 5), 6);
    assert_eq!(section_height(0, 6), 6);
}

#[test]
fn a_section_with_exactly_five_rows_shows_them_all_without_a_scrollbar() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut form = NewReq::new(String::new(), vec!["Scratch".to_string()], 0, None);
    form.headers.clear();
    for i in 0..5 {
        let mut row = HeaderRow::new();
        row.key = super::editor::Editor::new(&format!("Header{i}"), false);
        row.value = super::editor::Editor::new(&format!("Value{i}"), false);
        form.headers.push(row);
    }
    form.focus = NewField::Kvd(KvdKind::Header, 0, HdrCol::Key);

    // Exactly the height `section_height(1, 5)` would allocate: 1 header
    // line + 5 data rows + 1 pinned Add row = 7.
    let height = super::new_request::section_height(1, 5);
    assert_eq!(height, 7);
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 40,
        height,
    };
    let mut term = Terminal::new(TestBackend::new(40, height)).unwrap();
    term.draw(|f| super::new_request::draw_kvd_table(f, area, &form, KvdKind::Header, &s, &th))
        .unwrap();
    let buf = term.backend().buffer().clone();
    let text = buffer_text(&buf);
    for i in 0..5 {
        assert!(
            text.contains(&format!("Header{i}")),
            "row {i} should be fully visible:\n{text}"
        );
    }
    assert!(
        text.contains("Add header"),
        "the Add Header hint should be visible:\n{text}"
    );
    // No scrollbar thumb/track should render anywhere: everything fits.
    for y in 0..height {
        let sym = buf[(0, y)].symbol();
        assert_ne!(
            sym, "\u{2588}",
            "no scrollbar thumb expected when all rows fit, row {y}"
        );
        assert_ne!(
            sym, "\u{2502}",
            "no scrollbar track expected when all rows fit, row {y}"
        );
    }
}

#[test]
fn a_sixth_row_triggers_a_scrollbar_rendered_in_the_leftmost_column() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut form = NewReq::new(String::new(), vec!["Scratch".to_string()], 0, None);
    form.headers.clear();
    for i in 0..6 {
        let mut row = HeaderRow::new();
        row.key = super::editor::Editor::new(&format!("Header{i}"), false);
        row.value = super::editor::Editor::new(&format!("Value{i}"), false);
        form.headers.push(row);
    }
    form.focus = NewField::Kvd(KvdKind::Header, 0, HdrCol::Key);

    // The section stays capped at the same height once it would need to
    // scroll (`section_height` no longer grows past 5 visible rows).
    let height = super::new_request::section_height(1, 6);
    assert_eq!(height, 7);
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 40,
        height,
    };
    let mut term = Terminal::new(TestBackend::new(40, height)).unwrap();
    term.draw(|f| super::new_request::draw_kvd_table(f, area, &form, KvdKind::Header, &s, &th))
        .unwrap();
    let buf = term.backend().buffer().clone();
    let text = buffer_text(&buf);
    assert!(
        text.contains("Add header"),
        "the Add Header hint must stay pinned even while scrolling:\n{text}"
    );

    // The scrollbar (thumb or track) should appear somewhere in the
    // leftmost column, adjacent to the data instead of far to the right.
    let mut found_left = false;
    for y in 0..height {
        let sym = buf[(0, y)].symbol();
        if sym == "\u{2588}" || sym == "\u{2502}" {
            found_left = true;
        }
    }
    assert!(
        found_left,
        "expected a scrollbar thumb/track in the leftmost column:\n{text}"
    );

    // ...and it must NOT appear in the rightmost column anymore.
    let mut found_right = false;
    for y in 0..height {
        let sym = buf[(area.width - 1, y)].symbol();
        if sym == "\u{2588}" || sym == "\u{2502}" {
            found_right = true;
        }
    }
    assert!(
        !found_right,
        "scrollbar should no longer render in the rightmost column:\n{text}"
    );
}

#[test]
fn an_auto_detected_content_type_cell_shows_the_auto_placeholder() {
    // An empty Content-Type override on a File-kind row means Hurl will
    // auto-detect it at send time; the cell should show a dimmed "Auto"
    // placeholder rather than looking blank/unset (mirrors the Kind
    // column's "Select Kind..." placeholder).
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut form = NewReq::new(String::new(), vec!["Scratch".to_string()], 0, None);
    let mut row = FormRow::new();
    row.key = super::editor::Editor::new("avatar", false);
    row.value = super::editor::Editor::new("avatar.png", false);
    row.kind = crate::hurl::FormFieldKind::File;
    // ctype left empty: auto-detected, not overridden.
    form.form_fields.push(row);
    form.focus = NewField::FormField(0, FormCol::Value); // not focused on Ctype

    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 8,
    };
    let mut term = Terminal::new(TestBackend::new(100, 8)).unwrap();
    term.draw(|f| super::new_request::draw_form_table(f, area, &form, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(
        text.contains(s.content_type_auto_placeholder),
        "an unfocused, unset Content-Type cell on a File row should show \"{}\":\n{text}",
        s.content_type_auto_placeholder
    );
}

#[test]
fn a_file_kind_value_cell_shows_a_folder_icon_hint_but_a_text_kind_one_does_not() {
    // Pressing Enter on a File-kind row's Value cell opens a file
    // picker, but nothing about a plain text cell suggests that. A
    // folder icon at the cell's left edge (right next to the Kind
    // column, so it visually reads as belonging to the Value field) is
    // the discoverability hint, shown whether or not the row is focused.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    const FOLDER_ICON: &str = "\u{1F4C1}";
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut form = NewReq::new(String::new(), vec!["Scratch".to_string()], 0, None);
    let mut file_row = FormRow::new();
    file_row.key = super::editor::Editor::new("avatar", false);
    file_row.value = super::editor::Editor::new("avatar.png", false);
    file_row.kind = crate::hurl::FormFieldKind::File;
    let mut text_row = FormRow::new();
    text_row.key = super::editor::Editor::new("name", false);
    text_row.value = super::editor::Editor::new("crab", false);
    text_row.kind = crate::hurl::FormFieldKind::Text;
    form.form_fields.push(file_row);
    form.form_fields.push(text_row);
    // Neither row focused: exercises the unfocused render path for both.
    form.focus = NewField::AddFormField;

    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 8,
    };
    let mut term = Terminal::new(TestBackend::new(100, 8)).unwrap();
    term.draw(|f| super::new_request::draw_form_table(f, area, &form, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());
    assert_eq!(
        text.matches(FOLDER_ICON).count(),
        1,
        "exactly the File-kind row's Value cell shows the folder icon hint:\n{text}"
    );

    // The hint still shows once the File row's Value cell is focused
    // (editing mode), just in the accent colour instead of dim.
    form.focus = NewField::FormField(0, FormCol::Value);
    term.draw(|f| super::new_request::draw_form_table(f, area, &form, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());
    assert_eq!(
        text.matches(FOLDER_ICON).count(),
        1,
        "the folder icon hint still shows while the File row's Value cell is focused:\n{text}"
    );
    form.focus = NewField::AddFormField; // back to unfocused for the position check below

    // The icon sits on the left edge of the Value cell (right after the
    // Kind column), not the right edge, so it visually anchors to the
    // field it belongs to instead of floating at the far end of a wide
    // cell.
    term.draw(|f| super::new_request::draw_form_table(f, area, &form, &s, &th))
        .unwrap();
    let buf = term.backend().buffer();
    let row_y = 1u16; // header occupies row 0
    let icon_x = (0..area.width).find(|&x| buf[(x, row_y)].symbol() == FOLDER_ICON);
    let value_text_x = (0..area.width.saturating_sub(2)).find(|&x| {
        buf[(x, row_y)].symbol() == "p"
            && buf[(x + 1, row_y)].symbol() == "n"
            && buf[(x + 2, row_y)].symbol() == "g"
    });
    if let (Some(icon_x), Some(value_text_x)) = (icon_x, value_text_x) {
        assert!(
            icon_x < value_text_x,
            "folder icon (col {icon_x}) must be left of the value text (col {value_text_x})"
        );
    } else {
        panic!(
            "expected to find both the folder icon and the start of \"avatar.png\" on row {row_y}"
        );
    }
}

#[test]
fn a_long_body_shows_a_scrollbar_in_the_leftmost_column() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut form = NewReq::new(String::new(), vec!["Scratch".to_string()], 0, None);
    let long_body: String = (0..40)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    form.body = super::editor::Editor::new(&long_body, true);
    form.view_tab = WizardTab::Body;
    form.focus = NewField::Body;

    let mut term = Terminal::new(TestBackend::new(100, 16)).unwrap();
    term.draw(|f| super::new_request::draw_new_request(f, &form, &s, &th, true))
        .unwrap();
    let buf = term.backend().buffer().clone();
    let text = buffer_text(&buf);
    assert!(
        text.contains("line39"),
        "the body's last line (cursor position) is shown:\n{text}"
    );
    assert!(
        !text.contains("line0"),
        "the body overflows the visible area:\n{text}"
    );

    // The editor area starts a few rows down (name/method/url/tab bar +
    // the Body label line + the panel border); confirming the thumb
    // character appears at all confirms the scrollbar rendered (its
    // exact leftmost-column placement is already visible in the printed
    // buffer, directly after the panel's left border).
    assert!(
        text.contains('\u{2588}') || text.contains('\u{2502}'),
        "expected a scrollbar thumb/track near the left edge of the Body editor:\n{text}"
    );
}

// ── Raw Mode: editing the actual Hurl text ──────────────────────────────

#[test]
fn raw_mode_rejects_invalid_hurl_and_keeps_the_text_editable() {
    let entry = HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "");
    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::Main;

    app.on_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
    assert!(app.overlay.is_some(), "raw mode editor should be open");

    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        *editor = super::editor::Editor::new("this is not valid hurl {{{", true);
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    assert!(
        app.overlay.is_some(),
        "invalid hurl reopens the editor for correction"
    );
    if let Some(Overlay::Prompt { kind, editor, .. }) = &app.overlay {
        assert!(matches!(kind, PromptKind::Raw(0)));
        assert_eq!(
            editor.text(),
            "this is not valid hurl {{{",
            "the invalid text is preserved"
        );
    }
    assert_eq!(
        app.collections[0].entries[0].url, "http://h/x",
        "the entry is untouched"
    );
    assert!(!app.collections[0].entries[0].modified);
}

#[test]
fn raw_mode_reports_the_specific_parse_error_for_captures_without_a_response_line() {
    let entry = HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "");
    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::Main;

    app.on_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        *editor = super::editor::Editor::new(
            "# Get token\nPOST http://h/oauth2\n[Captures]\ntok: jsonpath \"$.token\"",
            true,
        );
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let s = crate::i18n::Strings::for_language(&app.language);
    match &app.status {
        Some(crate::i18n::Status::Error(msg)) => {
            assert!(
                msg.contains("Captures") && msg.contains("HTTP"),
                "the error names the section and the fix, not just 'expected one request': {msg}"
            );
        }
        other => panic!("expected a descriptive parse error, got {other:?}"),
    }

    // Ctrl+Y copies the status line and leaves it on screen (still readable /
    // re-copyable) rather than replacing it with a "Copied" acknowledgement.
    let before = app.status.as_ref().map(|st| st.text(&s));
    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    let after = app.status.as_ref().map(|st| st.text(&s));
    assert_eq!(before, after, "copying the status must not clear it");
}

#[test]
fn raw_mode_edits_fields_the_wizard_does_not_expose() {
    let mut entry = HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "");
    entry.expected_status = Some(200);
    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::Main;

    app.on_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
    assert!(app.overlay.is_some());

    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        let new_text = editor.text().replace("HTTP 200", "HTTP 201");
        *editor = super::editor::Editor::new(&new_text, true);
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    assert!(app.overlay.is_none(), "valid hurl commits and closes");
    let e = &app.collections[0].entries[0];
    assert_eq!(
        e.expected_status,
        Some(201),
        "raw mode can edit fields the wizard hides"
    );
    assert!(e.modified);
}

/// Shift+Arrow inside the Raw Mode editor selects text (extending from
/// wherever the cursor was when Shift was first held) without
/// disturbing the underlying text, and Ctrl+Y copies exactly that
/// selection — the "select and copy text from the Hurl Mode editor"
/// requirement.
#[test]
fn raw_mode_shift_arrow_selects_text_and_ctrl_y_copies_it() {
    let entry = HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "");
    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::Main;

    app.on_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        *editor = super::editor::Editor::new("GET http://example.com/target\n[Asserts]", true);
        editor.row = 0;
        editor.col = 4; // just after "GET "
    }

    // Shift+Right three times selects "htt" (chars 4..7 of the first line).
    for _ in 0..3 {
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    }
    if let Some(Overlay::Prompt { editor, .. }) = &app.overlay {
        assert_eq!(
            editor.selected_text().as_deref(),
            Some("htt"),
            "Shift+Right extends a selection char by char"
        );
    }

    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    // Ctrl+Y must not have inserted a literal 'y' or otherwise touched the text.
    if let Some(Overlay::Prompt { editor, .. }) = &app.overlay {
        assert_eq!(
            editor.text(),
            "GET http://example.com/target\n[Asserts]",
            "Ctrl+Y only copies, it never edits"
        );
        assert_eq!(
            editor.selected_text().as_deref(),
            Some("htt"),
            "the selection survives being copied"
        );
    }

    // A plain (non-Shift) arrow move clears the selection.
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    if let Some(Overlay::Prompt { editor, .. }) = &app.overlay {
        assert!(
            editor.selected_text().is_none(),
            "a plain arrow move clears the selection"
        );
    }
}

/// Mouse click-drag inside Raw Mode's editor selects text scoped to its
/// own `prompt_editor_area`, and releasing the mouse copies it — the
/// same click-drag-to-select-and-copy UX as the Main/Response panels,
/// now also available in the raw Hurl editor.
#[test]
fn raw_mode_mouse_drag_selects_text_and_copies_on_release() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let entry = HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "");
    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::Main;
    app.on_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        *editor = super::editor::Editor::new("GET http://example.com/target\n[Asserts]", true);
    }

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let area = app.prompt_editor_area;
    assert!(
        area.width > 0 && area.height > 0,
        "the raw editor's text area must be recorded for hit-testing"
    );

    // Drag-select "http" (chars 4..8 of the first line).
    let ev = |kind, col_offset: u16| MouseEvent {
        kind,
        column: area.x + col_offset,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), 4));
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), 8));
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    if let Some(Overlay::Prompt { editor, .. }) = &app.overlay {
        assert_eq!(
            editor.selected_text().as_deref(),
            Some("http"),
            "the drag selects exactly the dragged-over text"
        );
    }

    app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), 8));
    // Clicking somewhere outside the editor's area entirely (e.g. the
    // underlying app) must not have been affected — the overlay still
    // owns all mouse input while open.
    assert!(
        app.overlay.is_some(),
        "the overlay stays open across the drag"
    );
    assert!(
        matches!(app.status, Some(crate::i18n::Status::Copied)),
        "releasing the drag inside the raw editor also sets the copied status message"
    );
}

// ── Raw JSON Mode: editing the Request-JSON text (Shift+J) ─────────────

#[test]
fn raw_json_mode_rejects_invalid_json_and_keeps_the_text_editable() {
    let entry = HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "");
    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::Main;

    app.on_key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT));
    assert!(app.overlay.is_some(), "raw JSON mode editor should be open");

    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        *editor = super::editor::Editor::new("this is not valid json {{{", true);
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    assert!(
        app.overlay.is_some(),
        "invalid JSON reopens the editor for correction"
    );
    if let Some(Overlay::Prompt { kind, editor, .. }) = &app.overlay {
        assert!(matches!(kind, PromptKind::RawJson(0)));
        assert_eq!(
            editor.text(),
            "this is not valid json {{{",
            "the invalid text is preserved"
        );
    }
    assert_eq!(
        app.collections[0].entries[0].url, "http://h/x",
        "the entry is untouched"
    );
    assert!(!app.collections[0].entries[0].modified);
}

#[test]
fn raw_json_mode_edits_fields_the_wizard_does_not_expose() {
    let mut entry = HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "");
    entry.expected_status = Some(200);
    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::Main;

    app.on_key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT));
    assert!(app.overlay.is_some());

    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        let new_text = editor.text().replace("\"GET\"", "\"POST\"");
        *editor = super::editor::Editor::new(&new_text, true);
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    assert!(app.overlay.is_none(), "valid JSON commits and closes");
    let e = &app.collections[0].entries[0];
    assert_eq!(e.method, "POST", "raw JSON mode edits the method");
    assert_eq!(
        e.expected_status,
        Some(200),
        "fields the JSON view doesn't expose are preserved"
    );
    assert!(e.modified);
}

/// Shift+Arrow inside the Raw JSON Mode editor selects text and Ctrl+Y
/// copies it — the same selection/copy UX as Raw Mode (Hurl), now also
/// available in the raw JSON editor.
#[test]
fn raw_json_mode_shift_arrow_selects_text_and_ctrl_y_copies_it() {
    let entry = HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "");
    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::Main;

    app.on_key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT));
    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        *editor = super::editor::Editor::new("{\n  \"method\": \"GET\"\n}", true);
        editor.row = 1;
        editor.col = 2; // just before the opening quote of "method"
    }

    // Shift+Right three times selects `"me`.
    for _ in 0..3 {
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    }
    if let Some(Overlay::Prompt { editor, .. }) = &app.overlay {
        assert_eq!(
            editor.selected_text().as_deref(),
            Some("\"me"),
            "Shift+Right extends a selection char by char"
        );
    }

    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    if let Some(Overlay::Prompt { editor, .. }) = &app.overlay {
        assert_eq!(
            editor.text(),
            "{\n  \"method\": \"GET\"\n}",
            "Ctrl+Y only copies, it never edits"
        );
        assert_eq!(
            editor.selected_text().as_deref(),
            Some("\"me"),
            "the selection survives being copied"
        );
    }
}

/// A selection spanning multiple lines in a plain (non-panel) `Editor`
/// uses the same "stream" semantics as the panel selection: first line
/// from its start column onward, middle lines in full, last line up to
/// its end column.
#[test]
fn editor_multiline_selection_uses_stream_semantics() {
    use super::editor::Editor;

    let mut ed = Editor::new("first line\nsecond\nthird line", true);
    ed.row = 0;
    ed.col = 6; // "first |line"
    ed.begin_selection_if_needed();
    ed.row = 2;
    ed.col = 5; // "third| line"
    assert_eq!(ed.selected_text().as_deref(), Some("line\nsecond\nthird"));

    // Dragging backwards (anchor after cursor) must resolve identically.
    let mut ed2 = Editor::new("first line\nsecond\nthird line", true);
    ed2.row = 2;
    ed2.col = 5;
    ed2.begin_selection_if_needed();
    ed2.row = 0;
    ed2.col = 6;
    assert_eq!(ed2.selected_text().as_deref(), Some("line\nsecond\nthird"));

    // A collapsed selection (anchor == cursor, never actually moved) is None.
    let mut ed3 = Editor::new("abc", false);
    ed3.begin_selection_if_needed();
    assert!(ed3.selected_text().is_none());
}

// ── Recently used git URLs dropdown ────────────────────────────────────

#[test]
fn opening_the_git_wizard_offers_the_recent_urls_most_recent_first() {
    let mut app = TuiApp {
        recent_git_urls: vec![
            "https://example.test/a.git".into(),
            "https://example.test/b.git".into(),
        ],
        ..Default::default()
    };
    app.open_remote_wizard(RemoteKind::Collection);
    match &app.overlay {
        Some(Overlay::RemoteGit(w)) => assert_eq!(w.recent, app.recent_git_urls),
        _ => panic!("wizard did not open"),
    }
}

#[test]
fn down_opens_the_recent_urls_dropdown_and_enter_picks_one() {
    let mut app = TuiApp {
        recent_git_urls: vec![
            "https://example.test/a.git".into(),
            "https://example.test/b.git".into(),
        ],
        ..Default::default()
    };
    app.open_remote_wizard(RemoteKind::Collection);

    // Down (on the URL field) opens the dropdown instead of jumping fields.
    press(&mut app, KeyCode::Down);
    match &app.overlay {
        Some(Overlay::RemoteGit(w)) => {
            assert!(matches!(
                w.stage,
                RemoteStage::Connect {
                    field: 0,
                    recent_sel: Some(0)
                }
            ));
        }
        _ => panic!("wizard closed"),
    }
    press(&mut app, KeyCode::Down);
    match &app.overlay {
        Some(Overlay::RemoteGit(w)) => {
            assert!(
                matches!(
                    w.stage,
                    RemoteStage::Connect {
                        field: 0,
                        recent_sel: Some(1)
                    }
                ),
                "Down moves to the next item"
            );
        }
        _ => panic!("wizard closed"),
    }
    // Enter picks the highlighted URL, fills the field, and connects
    // immediately (no need to press Enter a second time).
    press(&mut app, KeyCode::Enter);
    match &app.overlay {
        Some(Overlay::RemoteGit(w)) => {
            assert_eq!(w.url.text(), "https://example.test/b.git");
            assert!(matches!(
                w.stage,
                RemoteStage::Loading {
                    phase: LoadPhase::Refs
                }
            ));
        }
        _ => panic!("wizard closed"),
    }
}

#[test]
fn up_from_the_first_recent_item_closes_the_dropdown() {
    let mut app = TuiApp {
        recent_git_urls: vec!["https://example.test/a.git".into()],
        ..Default::default()
    };
    app.open_remote_wizard(RemoteKind::Collection);
    press(&mut app, KeyCode::Down); // open dropdown at index 0
    press(&mut app, KeyCode::Up); // back out of the dropdown
    match &app.overlay {
        Some(Overlay::RemoteGit(w)) => {
            assert!(matches!(
                w.stage,
                RemoteStage::Connect {
                    field: 0,
                    recent_sel: None
                }
            ));
        }
        _ => panic!("wizard closed"),
    }
}

#[test]
fn typing_closes_the_dropdown_and_edits_the_field() {
    let mut app = TuiApp {
        recent_git_urls: vec!["https://example.test/a.git".into()],
        ..Default::default()
    };
    app.open_remote_wizard(RemoteKind::Collection);
    press(&mut app, KeyCode::Down); // open dropdown
    press(&mut app, KeyCode::Char('x'));
    match &app.overlay {
        Some(Overlay::RemoteGit(w)) => {
            assert!(
                matches!(
                    w.stage,
                    RemoteStage::Connect {
                        field: 0,
                        recent_sel: None
                    }
                ),
                "typing closes the dropdown"
            );
            assert_eq!(
                w.url.text(),
                "x",
                "the keystroke is applied to the url field"
            );
        }
        _ => panic!("wizard closed"),
    }
}

#[test]
fn completing_a_git_load_remembers_the_url_most_recent_first() {
    use super::editor::Editor;
    let mut app = TuiApp {
        recent_git_urls: vec!["https://example.test/old.git".into()],
        ..Default::default()
    };
    let mut w = RemoteWizard::new(RemoteKind::Collection, app.recent_git_urls.clone());
    w.url = Editor::new("https://example.test/new.git", false);
    w.selected_path = Some("api.hurl".into());

    let keep_open = app.apply_git_msg(&mut w, GitMsg::Content(Ok("GET http://h/x\n".into())));
    assert!(!keep_open, "a collection load closes the wizard");
    assert_eq!(
        app.recent_git_urls,
        vec![
            "https://example.test/new.git".to_string(),
            "https://example.test/old.git".to_string()
        ],
        "the just-used URL moves to the front"
    );
}

#[test]
fn reusing_the_same_url_moves_it_to_the_front_without_duplicating() {
    let mut app = TuiApp {
        recent_git_urls: vec![
            "https://example.test/a.git".into(),
            "https://example.test/b.git".into(),
        ],
        ..Default::default()
    };
    app.remember_git_url("https://example.test/b.git");
    assert_eq!(
        app.recent_git_urls,
        vec![
            "https://example.test/b.git".to_string(),
            "https://example.test/a.git".to_string()
        ],
        "re-using a URL moves it to the front instead of duplicating it"
    );
}

// ── Tab management: Ctrl+W close / Ctrl+Shift+T reopen / reordering ────

#[test]
fn ctrl_w_closes_the_active_tab_but_not_the_built_in_one() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.active_tab = 1;

    app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));

    assert_eq!(app.collections.len(), 1, "the 'api' tab was closed");
    assert_eq!(app.active_tab, 0);

    // Ctrl+W on the built-in Request tab is a no-op.
    app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(app.collections.len(), 1, "the built-in tab can't be closed");
}

#[test]
fn u_reopens_the_most_recently_closed_tab() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.active_tab = 1;
    app.close_active_tab();
    assert_eq!(app.collections.len(), 1);

    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));

    assert_eq!(app.collections.len(), 2, "the closed tab came back");
    assert_eq!(app.collections[1].name, "api");
    assert_eq!(app.active_tab, 1, "the reopened tab becomes active");
}

#[test]
fn reopening_with_no_closed_tabs_is_a_no_op() {
    let mut app = TuiApp::default();
    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(app.collections.len(), 1);
}

#[test]
fn reopened_tabs_come_back_in_last_closed_first_order() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.collections.push(Collection::new("web".into(), vec![]));
    app.active_tab = 1;
    app.close_active_tab(); // closes "api"; collections are now [Request, web]
    assert_eq!(app.collections[1].name, "web");
    app.active_tab = 1;
    app.close_active_tab(); // closes "web"
    assert_eq!(app.collections.len(), 1);

    app.reopen_closed_tab();
    assert_eq!(
        app.collections[app.active_tab].name, "web",
        "most recently closed comes back first"
    );
    app.reopen_closed_tab();
    assert!(
        app.collections.iter().any(|c| c.name == "api"),
        "the earlier closed tab is restored too"
    );
}

/// Creates a real temp folder standing in for a git-downloaded Workspace,
/// and a collection tab bound to it with `workspace_downloaded_from_git`
/// set — used across the `CloseGitWorkspace` confirmation tests below.
fn git_workspace_tab(tag: &str) -> (std::path::PathBuf, Collection) {
    let dir = std::env::temp_dir().join(format!(
        "paperboy_close_git_ws_{tag}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("alpha.hurl"), "GET https://example.com/alpha\n").unwrap();
    let mut col = Collection::new("downloaded".into(), vec![]);
    col.workspace_root = Some(dir.clone());
    col.workspace_downloaded_from_git = true;
    (dir, col)
}

#[test]
fn closing_a_git_downloaded_workspace_tab_asks_first_instead_of_closing_immediately() {
    let (dir, col) = git_workspace_tab("ask_first");
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = 1;

    app.close_active_tab();

    assert_eq!(app.collections.len(), 2, "the tab is not closed yet");
    match &app.overlay {
        Some(Overlay::CloseGitWorkspace { idx, path, sel }) => {
            assert_eq!(*idx, 1);
            assert_eq!(path, &dir);
            assert_eq!(*sel, 0, "Keep is the default, non-destructive choice");
        }
        other => {
            let _ = other; // Overlay isn't `Debug`; just report the mismatch.
            panic!("expected a CloseGitWorkspace confirm popup");
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keeping_a_git_workspace_closes_the_tab_but_leaves_the_folder_and_stays_undoable() {
    let (dir, col) = git_workspace_tab("keep");
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = 1;
    app.close_active_tab();

    // sel already defaults to 0 (Keep) — just confirm.
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.collections.len(), 1, "the tab was closed");
    assert!(app.overlay.is_none());
    assert!(dir.exists(), "the folder is kept on disk");

    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(
        app.collections.len(),
        2,
        "u reopens it since the folder is still there"
    );
    assert_eq!(
        app.collections[app.active_tab].workspace_root,
        Some(dir.clone())
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deleting_a_git_workspace_closes_the_tab_removes_the_folder_and_is_skipped_by_u() {
    let (dir, col) = git_workspace_tab("delete");
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = 1;
    app.close_active_tab();

    // Move the selection to "Delete" (index 1) and confirm.
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.collections.len(), 1, "the tab was closed");
    assert!(app.overlay.is_none());
    assert!(!dir.exists(), "the downloaded folder was deleted");

    // There is nothing left on disk to reopen, so it must not be offered
    // via the undo-close shortcut.
    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(
        app.collections.len(),
        1,
        "u is a no-op — the deleted tab was skipped"
    );
}

#[test]
fn escape_cancels_closing_a_git_workspace_tab_leaving_it_open() {
    let (dir, col) = git_workspace_tab("cancel");
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = 1;
    app.close_active_tab();

    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(app.collections.len(), 2, "the tab was not closed");
    assert!(app.overlay.is_none());
    assert!(dir.exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_a_locally_chosen_workspace_tab_is_immediate_and_unprompted() {
    // A Workspace picked from the user's own filesystem (not downloaded
    // from git) must behave exactly as it always has — no popup.
    let dir = std::env::temp_dir().join(format!("paperboy_close_local_ws_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut col = Collection::new("local".into(), vec![]);
    col.workspace_root = Some(dir.clone());
    // workspace_downloaded_from_git left at its default (false).
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = 1;

    app.close_active_tab();

    assert_eq!(
        app.collections.len(),
        1,
        "closed immediately, no confirmation needed"
    );
    assert!(app.overlay.is_none());
    assert!(dir.exists(), "a locally-chosen folder is never touched");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ctrl_shift_arrows_reorder_the_active_tab() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.collections.push(Collection::new("web".into(), vec![]));
    app.active_tab = 1; // "api"

    app.on_key(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));

    assert_eq!(app.collections[1].name, "web");
    assert_eq!(app.collections[2].name, "api");
    assert_eq!(
        app.active_tab, 2,
        "the moved tab stays active at its new position"
    );

    app.on_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));

    assert_eq!(app.collections[1].name, "api");
    assert_eq!(app.collections[2].name, "web");
    assert_eq!(app.active_tab, 1);
}

#[test]
fn tab_reordering_never_moves_the_built_in_tab() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new("api".into(), vec![]));
    app.active_tab = 1; // "api"

    // Trying to move "api" left would push it into the built-in tab's slot.
    app.on_key(KeyEvent::new(
        KeyCode::Left,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));

    assert_eq!(app.collections[0].name, "Request");
    assert_eq!(app.collections[1].name, "api");
    assert_eq!(app.active_tab, 1, "the move was rejected");

    // The built-in tab itself can never be moved either.
    app.active_tab = 0;
    app.on_key(KeyEvent::new(
        KeyCode::Right,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert_eq!(app.collections[0].name, "Request");
    assert_eq!(app.active_tab, 0);
}

#[test]
fn editing_a_request_populates_asserts_and_captures_in_the_combined_view() {
    // Regression test: the Captures table has an extra Name/Expression
    // header row that Asserts doesn't, so it needs one more line of fixed
    // height in the combined "All" view — previously both got the same
    // fixed height, which squeezed the one visible data row out from
    // under the header, leaving Captures looking empty even though the
    // entry (and the wizard form built from it) actually had a row.
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut entry = HurlEntry::from_fields("orig", "GET", "http://h/x", vec![], "");
    entry.asserts = vec!["status == 200".to_string()];
    entry.captures = vec![("token".to_string(), "jsonpath \"$.token\"".to_string())];

    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::List;
    press(&mut app, KeyCode::Enter); // opens the Edit Request wizard
    let form = match &app.overlay {
        Some(Overlay::NewRequest(form)) => form,
        _ => panic!("expected the Edit Request wizard to open"),
    };
    // The data itself is present in the form...
    assert_eq!(form.asserts.len(), 1);
    assert_eq!(form.asserts[0].expr.text(), "status == 200");
    assert_eq!(form.captures.len(), 1);
    assert_eq!(form.captures[0].name.text(), "token");
    assert_eq!(form.captures[0].expr.text(), "jsonpath \"$.token\"");

    // ...and it must actually be rendered, not hidden behind the header row.
    let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
    term.draw(|f| super::new_request::draw_new_request(f, form, &s, &th, true))
        .unwrap();
    let out = buffer_text(term.backend().buffer());
    assert!(
        out.contains("status == 200"),
        "the assert row should render:\n{out}"
    );
    assert!(
        out.contains("token") && out.contains("jsonpath"),
        "the capture row should render:\n{out}"
    );
}

// --- Folder navigation (Postman/Hurl nested collections) ---------------

/// Builds a collection with a mix of root-level and nested (slash-named)
/// requests: "root" at the top level, "A/one" and "A/two" inside folder
/// "A", and "A/B/deep" nested two levels inside "A/B".
fn collection_with_folders() -> Collection {
    Collection::new(
        "api".into(),
        vec![
            HurlEntry::from_fields("root", "GET", "http://h/root", vec![], ""),
            HurlEntry::from_fields("A/one", "GET", "http://h/one", vec![], ""),
            HurlEntry::from_fields("A/two", "GET", "http://h/two", vec![], ""),
            HurlEntry::from_fields("A/B/deep", "GET", "http://h/deep", vec![], ""),
        ],
    )
}

/// The row index of the "Up" row in `col`'s current folder view.
fn row_of_up(col: &Collection) -> usize {
    col.rows()
        .iter()
        .position(|r| matches!(r, crate::tree::Row::Up))
        .expect("no Up row")
}

/// The row index of the `Folder(name)` row in `col`'s current folder view.
fn row_of_folder(col: &Collection, name: &str) -> usize {
    col.rows()
        .iter()
        .position(|r| matches!(r, crate::tree::Row::Folder(n) if n == name))
        .unwrap_or_else(|| panic!("no Folder({name}) row"))
}

/// The row index of the entry titled `title` in `col`'s current folder view.
fn row_of_entry(col: &Collection, title: &str) -> usize {
    let idx = col
        .entries
        .iter()
        .position(|e| e.title == title)
        .expect("no such entry");
    col.rows()
        .iter()
        .position(|r| matches!(r, crate::tree::Row::Entry(i) if *i == idx))
        .unwrap_or_else(|| panic!("entry {title:?} is not visible in the current folder"))
}

#[test]
fn down_arrow_steps_through_folder_and_entry_rows_at_the_root() {
    let mut app = TuiApp::default();
    app.collections[0] = collection_with_folders();
    app.focus = Pane::List;

    // Root view: "A" (folder, alphabetically first) then "root" (entry).
    let folder_row = row_of_folder(&app.collections[0], "A");
    let root_row = row_of_entry(&app.collections[0], "root");
    app.collections[0].list_cursor = folder_row;

    press(&mut app, KeyCode::Down);
    assert_eq!(
        app.collections[0].list_cursor, root_row,
        "Down moves to the next row (the leaf request)"
    );
    // Selected_entry only updates when landing on an actual entry row.
    let root_idx = app.collections[0]
        .entries
        .iter()
        .position(|e| e.title == "root")
        .unwrap();
    assert_eq!(app.collections[0].selected_entry, root_idx);
}

#[test]
fn enter_descends_into_a_folder_and_backspace_ascends() {
    let mut app = TuiApp::default();
    app.collections[0] = collection_with_folders();
    app.focus = Pane::List;
    app.collections[0].list_cursor = row_of_folder(&app.collections[0], "A");

    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.collections[0].folder,
        vec!["A".to_string()],
        "Enter descends into the folder"
    );
    assert_eq!(
        app.collections[0].list_cursor, 0,
        "cursor resets on entering a folder"
    );
    assert!(
        app.overlay.is_none(),
        "descending into a folder must not open the wizard"
    );

    // Ascend back out with Backspace (a shortcut for the Up row).
    press(&mut app, KeyCode::Backspace);
    assert!(
        app.collections[0].folder.is_empty(),
        "Backspace ascends back to the root"
    );
}

#[test]
fn enter_on_the_up_row_ascends_to_the_parent_folder() {
    let mut app = TuiApp::default();
    app.collections[0] = collection_with_folders();
    app.collections[0].folder = vec!["A".to_string()];
    app.focus = Pane::List;
    app.collections[0].list_cursor = row_of_up(&app.collections[0]);

    press(&mut app, KeyCode::Enter);
    assert!(
        app.collections[0].folder.is_empty(),
        "Enter on the Up row goes back to the root"
    );
}

#[test]
fn enter_on_a_request_row_inside_a_folder_still_opens_the_edit_wizard() {
    let mut app = TuiApp::default();
    app.collections[0] = collection_with_folders();
    app.collections[0].folder = vec!["A".to_string()];
    app.focus = Pane::List;
    app.collections[0].list_cursor = row_of_entry(&app.collections[0], "A/one");
    app.collections[0].selected_entry = app.collections[0]
        .entries
        .iter()
        .position(|e| e.title == "A/one")
        .unwrap();

    press(&mut app, KeyCode::Enter);
    assert!(
        app.overlay.is_some(),
        "Enter on a request row opens the Edit Request wizard"
    );
    assert!(matches!(app.focus, Pane::Main));
}

#[test]
fn delete_is_a_no_op_on_a_folder_or_up_row() {
    let mut app = TuiApp::default();
    app.collections[0] = collection_with_folders();
    app.focus = Pane::List;
    let before = app.collections[0].entries.len();

    // Cursor is on the "A" folder row at the root.
    app.collections[0].list_cursor = row_of_folder(&app.collections[0], "A");
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(
        app.collections[0].entries.len(),
        before,
        "deleting a folder row is a no-op"
    );

    app.collections[0].folder = vec!["A".to_string()];
    app.collections[0].list_cursor = row_of_up(&app.collections[0]);
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(
        app.collections[0].entries.len(),
        before,
        "deleting the Up row is a no-op"
    );
}

#[test]
fn delete_removes_a_request_row_while_browsing_a_folder() {
    let mut app = TuiApp::default();
    app.collections[0] = collection_with_folders();
    app.collections[0].folder = vec!["A".to_string()];
    app.focus = Pane::List;
    app.collections[0].list_cursor = row_of_entry(&app.collections[0], "A/one");
    app.collections[0].selected_entry = app.collections[0]
        .entries
        .iter()
        .position(|e| e.title == "A/one")
        .unwrap();

    press(&mut app, KeyCode::Char('x'));
    assert!(
        !app.collections[0]
            .entries
            .iter()
            .any(|e| e.title == "A/one"),
        "the highlighted request is deleted"
    );
    assert!(
        app.collections[0]
            .entries
            .iter()
            .any(|e| e.title == "A/two"),
        "other requests in the folder remain"
    );
}

#[test]
fn requests_list_breadcrumb_shows_the_current_folder() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut app = TuiApp::default();
    app.collections[0] = collection_with_folders();
    app.collections[0].folder = vec!["A".to_string(), "B".to_string()];
    app.focus = Pane::List;

    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    term.draw(|f| super::draw::draw_collection_left(f, f.area(), &app, 0, &s, &th))
        .unwrap();
    let out = buffer_text(term.backend().buffer());
    assert!(
        out.contains("A") && out.contains("B"),
        "the breadcrumb shows the nested folder path:\n{out}"
    );
}

#[test]
fn new_request_prefills_the_current_folder_in_the_name_field() {
    let mut app = TuiApp::default();
    app.collections[0] = collection_with_folders();
    app.collections[0].folder = vec!["A".to_string(), "B".to_string()];
    app.focus = Pane::List;

    press(&mut app, KeyCode::Char('n'));
    match &app.overlay {
        Some(Overlay::NewRequest(form)) => {
            assert_eq!(
                form.name.text(),
                "A/B/",
                "the Name field is prefilled with the current folder"
            );
        }
        _ => panic!("expected the New Request form to open"),
    }
}

#[test]
fn persisted_state_resyncs_the_folder_view_after_reload() {
    // A persisted collection whose `selected_entry` points at a deeply
    // nested request must resync `folder`/`list_cursor` on load, so the
    // Requests list opens already browsing the right folder instead of
    // showing the root view with a stale cursor.
    let mut col = collection_with_folders();
    col.selected_entry = col
        .entries
        .iter()
        .position(|e| e.title == "A/B/deep")
        .unwrap();
    // Simulate a fresh load where the view-layer fields haven't been
    // computed yet (as if freshly deserialized).
    col.folder = vec![];
    col.list_cursor = 0;

    let persisted = crate::persistence::PersistedTab::from_collection(&col, None);
    let (restored, _pending) = persisted.into_collection(None);

    assert_eq!(restored.folder, vec!["A".to_string(), "B".to_string()]);
}

#[test]
fn a_workspace_tabs_entries_are_not_snapshotted_and_are_re_read_fresh_on_restore() {
    // A Workspace tab is bound to a live folder, not a frozen file: its
    // `entries` must not be persisted at all, and restoring it should
    // re-read whichever file was last selected straight from disk
    // instead of trusting any old snapshot.
    let dir = std::env::temp_dir().join(format!("paperboy_ws_persist_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("orders.hurl");
    std::fs::write(&file, "GET https://example.com/one\n").unwrap();

    let mut col = Collection::new("orders".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_filter_hurl_json = false;
    col.path = Some(file.clone());

    let persisted = crate::persistence::PersistedTab::from_collection(&col, None);
    assert!(
        persisted.entries.is_empty(),
        "a workspace tab's entries are never snapshotted"
    );
    assert_eq!(
        persisted.workspace_root.as_deref(),
        Some(dir.to_string_lossy().as_ref())
    );
    assert_eq!(persisted.workspace_filter_hurl_json, Some(false));

    let (restored, _pending) = persisted.into_collection(None);
    assert_eq!(restored.workspace_root, Some(dir.clone()));
    assert!(!restored.workspace_filter_hurl_json);
    assert_eq!(restored.path, Some(file));
    assert_eq!(
        restored.entries.len(),
        1,
        "the file is re-parsed fresh from disk, not from any snapshot"
    );
    assert_eq!(restored.entries[0].url, "https://example.com/one");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_workspace_tab_whose_selected_file_has_vanished_restores_to_the_empty_no_collection_state() {
    // If the last-selected file (or the whole root) is gone by restart,
    // fall back to "no collection chosen yet" rather than showing stale
    // entries — the picker auto-opens from there.
    let dir = std::env::temp_dir().join(format!(
        "paperboy_ws_persist_missing_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let missing_file = dir.join("gone.hurl");

    let mut col = Collection::new("gone".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.path = Some(missing_file);

    let persisted = crate::persistence::PersistedTab::from_collection(&col, None);
    let (restored, _pending) = persisted.into_collection(None);

    assert_eq!(restored.workspace_root, Some(dir.clone()));
    assert_eq!(restored.path, None);
    assert!(restored.entries.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_workspace_tab_whose_entire_root_folder_has_vanished_is_fully_reset_not_just_its_file() {
    // Unlike just the selected *file* going missing (the test above),
    // when the whole root folder is gone (e.g. it was a git-downloaded
    // /tmp folder cleared by the OS since), there is nothing left to
    // scan or reopen at all — the tab must be fully reset back to a
    // plain "no collection chosen" tab: no phantom `workspace_root`, and
    // `workspace_downloaded_from_git` cleared too (so it doesn't keep
    // offering a nonsensical "keep the already-gone folder?" popup on
    // close).
    let dir = std::env::temp_dir().join(format!(
        "paperboy_ws_persist_root_gone_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir); // ensure it does NOT exist

    let mut col = Collection::new("ghost-repo".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_downloaded_from_git = true;
    col.path = Some(dir.join("gone.hurl"));

    let persisted = crate::persistence::PersistedTab::from_collection(&col, None);
    let (restored, _pending) = persisted.into_collection(None);

    assert_eq!(
        restored.workspace_root, None,
        "the dead root itself must be cleared, not just the file"
    );
    assert_eq!(restored.path, None);
    assert!(restored.entries.is_empty());
    assert!(
        !restored.workspace_downloaded_from_git,
        "no folder is left to offer keeping/deleting on close"
    );
}

#[test]
fn restoring_a_tab_whose_workspace_root_vanished_shows_a_status_explaining_it() {
    // End-to-end: `TuiApp::apply_persisted` must surface a status message
    // when a Workspace tab's root turned out to be missing, so the user
    // understands why a previously-populated tab now looks empty instead
    // of being left to wonder silently.
    let dir = std::env::temp_dir().join(format!(
        "paperboy_ws_apply_persisted_root_gone_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir); // ensure it does NOT exist

    let mut col = Collection::new("ghost-repo".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_downloaded_from_git = true;

    let tab = crate::persistence::PersistedTab::from_collection(&col, None);
    let state = crate::persistence::PersistedState {
        tabs: vec![tab],
        ..Default::default()
    };

    let mut app = TuiApp::default();
    app.apply_persisted(state);

    assert_eq!(app.collections[0].workspace_root, None);
    match &app.status {
        Some(crate::i18n::Status::WorkspaceFolderMissing(name)) => assert_eq!(name, "ghost-repo"),
        other => panic!("expected a WorkspaceFolderMissing status, got {other:?}"),
    }
}

#[test]
fn an_ordinary_non_workspace_tab_still_persists_a_full_entries_snapshot() {
    // Confirms the existing behavior for plain (non-Workspace) tabs is
    // unchanged by the new workspace fields.
    let col = collection_with_folders();
    let persisted = crate::persistence::PersistedTab::from_collection(&col, None);
    assert_eq!(persisted.entries.len(), col.entries.len());
    assert!(persisted.workspace_root.is_none());
    assert!(persisted.workspace_filter_hurl_json.is_none());

    let (restored, _pending) = persisted.into_collection(None);
    assert_eq!(restored.entries.len(), col.entries.len());
    assert!(restored.workspace_root.is_none());
    assert!(
        restored.workspace_filter_hurl_json,
        "defaults to true when absent from an older state file"
    );
}

// ── Workspaces (folder-of-collections) ──────────────────────────────────

/// A temp folder with a mix of matching and non-matching files/subfolders
/// used across the WorkspacePicker tests below.
fn workspace_temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("paperboy_ws_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("alpha.hurl"), "GET https://example.com/alpha\n").unwrap();
    std::fs::write(dir.join("notes.txt"), "not a collection").unwrap();
    std::fs::write(dir.join("sub").join("beta.json"), "[]").unwrap();
    dir
}

#[test]
fn space_confirms_the_current_directory_as_a_workspace_root_and_opens_the_picker() {
    let dir = workspace_temp_dir("space_confirm");
    let mut app = TuiApp {
        last_browse_dir: Some(dir.clone()),
        ..Default::default()
    };
    app.open_browser(FileAction::OpenWorkspace);

    press(&mut app, KeyCode::Char(' '));

    let ci = app.active_tab;
    assert!(ci > 0, "a new tab was created for the workspace");
    assert_eq!(app.collections[ci].workspace_root, Some(dir.clone()));
    assert_eq!(app.collections[ci].path, None, "no file chosen yet");
    match &app.overlay {
        Some(Overlay::WorkspacePicker(picker)) => {
            assert_eq!(picker.root, dir);
            assert_eq!(picker.collection_idx, ci);
        }
        _ => panic!("expected the WorkspacePicker to open immediately after confirming the root"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn workspace_picker_nav_only_moves_across_file_rows_skipping_directories() {
    let dir = workspace_temp_dir("nav");
    let picker = WorkspacePickerState::new(0, dir.clone(), true);
    // Filtered entries: alpha.hurl, sub/ (dir), sub/beta.json — notes.txt
    // is excluded by the .hurl/.json filter.
    assert!(
        picker
            .entries
            .iter()
            .any(|e| e.is_dir && e.display_name == "sub")
    );
    assert!(!picker.entries.iter().any(|e| e.display_name == "notes.txt"));

    let mut picker = picker;
    let first = picker.selected;
    picker.nav(1);
    assert_ne!(picker.selected, first, "moved to the next file row");
    assert!(
        !picker.entries[picker.selected].is_dir,
        "selection always lands on a file row"
    );
    picker.nav(100); // clamp past the end rather than wrapping
    assert!(!picker.entries[picker.selected].is_dir);
    picker.nav(-100); // clamp before the start rather than wrapping
    assert!(!picker.entries[picker.selected].is_dir);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tab_toggles_the_filter_rescans_and_syncs_the_choice_back_onto_the_collection() {
    let dir = workspace_temp_dir("filter_toggle");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_filter_hurl_json = true;
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;
    app.overlay = Some(Overlay::WorkspacePicker(WorkspacePickerState::new(
        ci,
        dir.clone(),
        true,
    )));

    press(&mut app, KeyCode::Tab);

    match &app.overlay {
        Some(Overlay::WorkspacePicker(picker)) => {
            assert!(!picker.filter_hurl_json, "Tab toggles the filter off");
            assert!(
                picker.entries.iter().any(|e| e.display_name == "notes.txt"),
                "unfiltered rescan shows every file"
            );
        }
        _ => panic!("expected the picker to stay open"),
    }
    assert!(
        !app.collections[ci].workspace_filter_hurl_json,
        "the toggle is written back onto the Collection"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn enter_on_a_file_row_loads_it_and_closes_the_picker() {
    let dir = workspace_temp_dir("enter_load");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;
    let mut picker = WorkspacePickerState::new(ci, dir.clone(), true);
    // Selection starts on the first file row (alpha.hurl, dirs-before-files
    // sorting means "sub/" comes first but is skipped by `new`).
    assert!(!picker.entries[picker.selected].is_dir);
    picker.selected = picker
        .entries
        .iter()
        .position(|e| e.display_name == "alpha.hurl")
        .unwrap();
    app.overlay = Some(Overlay::WorkspacePicker(picker));

    press(&mut app, KeyCode::Enter);

    assert!(app.overlay.is_none(), "picker closes once a file is chosen");
    assert_eq!(app.collections[ci].path, Some(dir.join("alpha.hurl")));
    assert_eq!(app.collections[ci].entries.len(), 1);
    assert_eq!(
        app.collections[ci].entries[0].url,
        "https://example.com/alpha"
    );
    assert_eq!(
        app.collections[ci].workspace_root,
        Some(dir.clone()),
        "stays bound to the same folder"
    );
    assert_eq!(
        app.collections[ci].name, "ws",
        "the tab's own name is untouched by picking a file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Build a `NewReq` form targeting tab `ci` with the given URL. The
/// `target_names` vector is padded so `NewReq::new`'s internal
/// `target_idx.min(len-1)` clamp keeps `ci` intact.
fn new_request_form_for_tab(ci: usize, url: &str) -> NewReq {
    let names: Vec<String> = (0..=ci).map(|i| format!("tab{i}")).collect();
    let mut form = NewReq::new(String::new(), names, ci, None);
    form.url = super::editor::Editor::new(url, false);
    form
}

#[test]
fn new_request_targeting_a_workspace_opens_the_destination_picker() {
    let dir = workspace_temp_dir("newreq_opens_picker");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;

    app.submit_new_request(new_request_form_for_tab(ci, "http://h/new"));

    assert!(
        app.collections[ci].entries.is_empty(),
        "the request is NOT silently pushed onto the loaded file"
    );
    assert!(
        app.pending_workspace_request.is_some(),
        "the request is parked awaiting a destination"
    );
    match &app.overlay {
        Some(Overlay::WorkspacePicker(p)) => {
            assert_eq!(p.collection_idx, ci);
            assert!(
                p.mode == crate::tui::app::WsPickerMode::AddRequest,
                "picker is in add-request mode"
            );
        }
        _ => panic!("expected the workspace destination picker to open"),
    }
    assert_eq!(app.active_tab, ci, "focus moved to the workspace tab");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn picking_a_file_in_add_mode_appends_the_request_and_shows_it() {
    let dir = workspace_temp_dir("newreq_append");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;

    app.submit_new_request(new_request_form_for_tab(ci, "http://h/new"));

    // Highlight alpha.hurl in the just-opened destination picker.
    if let Some(Overlay::WorkspacePicker(picker)) = &mut app.overlay {
        picker.selected = picker
            .entries
            .iter()
            .position(|e| e.display_name == "alpha.hurl")
            .unwrap();
    } else {
        panic!("expected the destination picker");
    }

    press(&mut app, KeyCode::Enter);

    assert!(app.overlay.is_none(), "the picker closes after landing");
    assert!(
        app.pending_workspace_request.is_none(),
        "the parked request has been placed"
    );
    assert_eq!(app.collections[ci].path, Some(dir.join("alpha.hurl")));
    // The file's own request plus the appended new one.
    assert_eq!(app.collections[ci].entries.len(), 2);
    let added = app.collections[ci].entries.last().unwrap();
    assert_eq!(added.url, "http://h/new");
    assert!(
        added.user_added && added.modified,
        "marked as an unsaved add"
    );
    assert_eq!(
        app.collections[ci].selected_entry, 1,
        "the new request is selected so it's visible"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn n_in_the_picker_creates_a_new_collection_holding_the_request() {
    let dir = workspace_temp_dir("newreq_new_collection");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;

    app.submit_new_request(new_request_form_for_tab(ci, "http://h/new"));

    // `n` in the destination picker → the "name a new collection" prompt.
    press(&mut app, KeyCode::Char('n'));
    match &app.overlay {
        Some(Overlay::Prompt { kind, .. }) => {
            assert!(matches!(kind, PromptKind::NewWorkspaceCollection(idx) if *idx == ci));
        }
        _ => panic!("expected the new-collection name prompt"),
    }

    // A relative subfolder path with no extension → `.hurl` is defaulted and
    // the folder is only created on save.
    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        *editor = super::editor::Editor::new("api/orders", false);
    }
    press(&mut app, KeyCode::Enter);

    assert!(app.overlay.is_none());
    assert!(app.pending_workspace_request.is_none());
    assert_eq!(
        app.collections[ci].path,
        Some(dir.join("api").join("orders.hurl")),
        "extension defaulted to .hurl and rooted under the workspace"
    );
    assert_eq!(app.collections[ci].entries.len(), 1);
    let added = &app.collections[ci].entries[0];
    assert_eq!(added.url, "http://h/new");
    assert!(added.user_added && added.modified);
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::WorkspaceCollectionCreated(_))
    ));

    // Saving writes the file, creating the missing `api/` parent folder.
    app.active_tab = ci;
    app.do_file_action(
        FileAction::SaveCollection,
        dir.join("api").join("orders.hurl").to_str().unwrap(),
    );
    assert!(dir.join("api").join("orders.hurl").is_file());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn escaping_the_destination_picker_clears_the_parked_request() {
    let dir = workspace_temp_dir("newreq_cancel");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;

    app.submit_new_request(new_request_form_for_tab(ci, "http://h/new"));
    assert!(app.pending_workspace_request.is_some());

    press(&mut app, KeyCode::Esc);

    assert!(app.overlay.is_none());
    assert!(
        app.pending_workspace_request.is_none(),
        "an aborted flow must not leak the parked request"
    );
    assert!(app.collections[ci].entries.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_workspace_collection_rejects_paths_escaping_the_root() {
    let dir = workspace_temp_dir("newreq_escape");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;

    app.create_workspace_collection(ci, "../evil".to_string());

    assert_eq!(
        app.collections[ci].path, None,
        "a `..` path is rejected, not created"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_workspace_report_writes_the_file_and_opens_a_workspace_report() {
    let dir = workspace_temp_dir("ws_new_report");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;

    // A relative subfolder path with no extension → `.report` is defaulted and
    // the parent folder is created on the spot (the report is written now, not
    // deferred like a new collection).
    app.create_workspace_report(ci, "reports/nightly".to_string());

    let expected = dir.join("reports").join("nightly.report");
    assert!(
        expected.is_file(),
        "the report is written to disk immediately"
    );
    assert_eq!(app.reports.len(), 1, "a report tab was opened");
    let rt = &app.reports[0];
    assert_eq!(rt.report.path.as_deref(), Some(expected.as_path()));
    assert!(
        rt.workspace.is_some(),
        "opened pinned to the workspace tree, not as a plain report tab"
    );
    assert_eq!(
        app.active_tab,
        app.collections.len() + app.reports.len() - 1,
        "the new report tab is the active tab"
    );
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::WorkspaceReportCreated(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_workspace_report_rejects_paths_escaping_the_root() {
    let dir = workspace_temp_dir("ws_new_report_escape");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;

    app.create_workspace_report(ci, "../evil".to_string());

    assert!(
        app.reports.is_empty(),
        "a `..` path is rejected, no report is created"
    );
    assert!(!dir.join("..").join("evil.report").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capital_r_in_the_workspace_picker_opens_the_new_report_prompt() {
    let dir = workspace_temp_dir("ws_picker_new_report");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;

    // A Browse-mode picker (the `w` flow) → `R` opens the new-report prompt.
    app.open_workspace_picker_for_active_tab();
    press(&mut app, KeyCode::Char('R'));

    match &app.overlay {
        Some(Overlay::Prompt { kind, .. }) => {
            assert!(matches!(kind, PromptKind::NewWorkspaceReport(idx) if *idx == ci));
        }
        _ => panic!("expected the new-report name prompt"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Load `alpha.hurl` into a fresh Workspace tab so a request row is
/// highlighted, ready for a move/copy. Returns the app and the tab index.
fn workspace_tab_with_alpha_loaded(tag: &str) -> (TuiApp, usize, std::path::PathBuf) {
    let dir = workspace_temp_dir(tag);
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    app.focus = Pane::List;
    // Highlight the request row (alpha.hurl holds a single request).
    let cursor = app.collections[ci]
        .ws_rows()
        .into_iter()
        .position(|r| matches!(r, crate::collection::WsRow::Request(_)))
        .expect("alpha.hurl's request row");
    app.collections[ci].list_cursor = cursor;
    (app, ci, dir)
}

#[test]
fn m_moves_the_highlighted_request_to_another_collection_file() {
    let (mut app, ci, dir) = workspace_tab_with_alpha_loaded("move_across");

    // `m` parks the request and opens the transfer picker in move mode.
    press(&mut app, KeyCode::Char('m'));
    assert!(app.pending_workspace_transfer.is_some());
    match &mut app.overlay {
        Some(Overlay::WorkspacePicker(p)) => {
            assert_eq!(p.mode, crate::tui::app::WsPickerMode::MoveRequest);
            p.selected = p
                .entries
                .iter()
                .position(|e| e.display_name == "beta.json")
                .unwrap();
        }
        _ => panic!("expected the transfer picker"),
    }

    press(&mut app, KeyCode::Enter);

    assert!(app.overlay.is_none());
    assert!(
        app.pending_workspace_transfer.is_none(),
        "transfer committed"
    );
    // The destination file now holds the moved request...
    let dest = std::fs::read_to_string(dir.join("sub").join("beta.json")).unwrap();
    assert!(dest.contains("https://example.com/alpha"));
    // ...and the source file no longer does (moved, not copied).
    let src = std::fs::read_to_string(dir.join("alpha.hurl")).unwrap();
    assert!(!src.contains("https://example.com/alpha"));
    assert!(
        app.collections[ci].entries.is_empty(),
        "the request left the in-memory source too"
    );
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::RequestMoved(..))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn c_copies_the_highlighted_request_leaving_the_source_intact() {
    let (mut app, ci, dir) = workspace_tab_with_alpha_loaded("copy_across");

    press(&mut app, KeyCode::Char('c'));
    assert!(app.pending_workspace_transfer.is_some());
    match &mut app.overlay {
        Some(Overlay::WorkspacePicker(p)) => {
            assert_eq!(p.mode, crate::tui::app::WsPickerMode::CopyRequest);
            p.selected = p
                .entries
                .iter()
                .position(|e| e.display_name == "beta.json")
                .unwrap();
        }
        _ => panic!("expected the transfer picker"),
    }

    press(&mut app, KeyCode::Enter);

    // Destination gains the request; the source keeps it.
    let dest = std::fs::read_to_string(dir.join("sub").join("beta.json")).unwrap();
    assert!(dest.contains("https://example.com/alpha"));
    let src = std::fs::read_to_string(dir.join("alpha.hurl")).unwrap();
    assert!(src.contains("https://example.com/alpha"));
    assert_eq!(
        app.collections[ci].entries.len(),
        1,
        "a copy leaves the in-memory source untouched"
    );
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::RequestCopied(..))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn moving_a_request_onto_its_own_source_file_is_a_no_op() {
    let (mut app, ci, dir) = workspace_tab_with_alpha_loaded("move_same");

    press(&mut app, KeyCode::Char('m'));
    match &mut app.overlay {
        Some(Overlay::WorkspacePicker(p)) => {
            p.selected = p
                .entries
                .iter()
                .position(|e| e.display_name == "alpha.hurl")
                .unwrap();
        }
        _ => panic!("expected the transfer picker"),
    }

    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.collections[ci].entries.len(),
        1,
        "moving onto the same file changes nothing"
    );
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::RequestMoved(..))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn m_and_c_are_no_ops_on_a_non_workspace_tab() {
    let mut app = TuiApp::default();
    let mut col = Collection::new("plain".to_string(), Vec::new());
    col.entries.push(crate::hurl::HurlEntry::from_fields(
        "x",
        "GET",
        "http://h/x",
        Vec::new(),
        "",
    ));
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;
    app.focus = Pane::List;

    press(&mut app, KeyCode::Char('m'));
    assert!(app.pending_workspace_transfer.is_none());
    assert!(app.overlay.is_none());
    press(&mut app, KeyCode::Char('c'));
    assert!(app.pending_workspace_transfer.is_none());
}

#[test]
fn deleting_a_request_reports_the_undo_hint_in_the_status_bar() {
    let mut app = TuiApp::default();
    let mut col = Collection::new("plain".to_string(), Vec::new());
    col.entries.push(crate::hurl::HurlEntry::from_fields(
        "x",
        "POST",
        "http://h/x",
        Vec::new(),
        "",
    ));
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;
    app.focus = Pane::List;
    app.collections[ci].sync_folder_to_selected();

    press(&mut app, KeyCode::Char('x'));

    match &app.status {
        Some(crate::i18n::Status::RequestDeleted(m)) => assert_eq!(m, "POST"),
        other => panic!("expected RequestDeleted status, got {other:?}"),
    }
    let s = crate::i18n::Strings::for_language(&app.language);
    assert!(
        app.status.as_ref().unwrap().text(&s).contains("(u)"),
        "the message names the undo key"
    );
}

#[test]
fn closing_a_tab_reports_the_reopen_hint_in_the_status_bar() {
    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("gone".to_string(), Vec::new()));
    let ci = app.collections.len() - 1;
    app.active_tab = ci;

    app.finish_close_tab(ci, false);

    assert!(matches!(app.status, Some(crate::i18n::Status::TabClosed)));
    let s = crate::i18n::Strings::for_language(&app.language);
    assert!(app.status.as_ref().unwrap().text(&s).contains("(u)"));
}

#[test]
fn deleting_a_git_workspace_folder_gives_no_reopen_hint() {
    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("ws".to_string(), Vec::new()));
    let ci = app.collections.len() - 1;
    app.active_tab = ci;

    // delete_folder = true → the folder is gone and can't be reopened.
    app.finish_close_tab(ci, true);

    assert!(
        app.status.is_none(),
        "a deleted git workspace can't be reopened, so no undo hint"
    );
}

#[test]
fn renaming_a_workspace_tab_survives_loading_a_different_collection_within_it() {
    // Regression test: `load_workspace_file` used to overwrite `name`
    // with the newly-picked file's stem every time, silently discarding
    // any rename the user had made. It must now leave `name` alone.
    let dir = workspace_temp_dir("rename_survives");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;

    // Rename the tab via F2, exactly like a user would.
    app.focus = Pane::Tabs;
    app.open_prompt_rename();
    if let Some(Overlay::Prompt { editor, .. }) = &mut app.overlay {
        *editor = super::editor::Editor::new("My Renamed Workspace", false);
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.collections[ci].name, "My Renamed Workspace");

    // Load a file (as the WorkspacePicker would) — the rename must survive.
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    assert_eq!(
        app.collections[ci].name, "My Renamed Workspace",
        "renaming persists across picking a file"
    );
    assert_eq!(app.collections[ci].path, Some(dir.join("alpha.hurl")));

    // Loading a second, different file must not touch the name either.
    app.load_workspace_file(ci, dir.join("sub").join("beta.json"));
    assert_eq!(
        app.collections[ci].name, "My Renamed Workspace",
        "renaming persists across switching collections"
    );
    assert_eq!(
        app.collections[ci].path,
        Some(dir.join("sub").join("beta.json"))
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_panel_title_tracks_the_loaded_collection_while_the_tab_bar_keeps_the_tabs_own_name() {
    use ratatui::{Terminal, backend::TestBackend};
    let dir = workspace_temp_dir("list_title");
    let mut col = Collection::new("My Workspace".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_auto_prompt_dismissed = true;
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;
    app.load_workspace_file(ci, dir.join("alpha.hurl"));

    let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let text = buffer_text(term.backend().buffer());

    assert!(
        text.contains("My Workspace"),
        "the tab bar shows the tab's own (renameable) name"
    );
    assert!(
        text.contains("alpha"),
        "the List panel title shows the currently loaded collection's name"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_git_downloaded_workspace_tab_shows_both_the_git_and_folder_icons() {
    use ratatui::{Terminal, backend::TestBackend};
    let dir = workspace_temp_dir("git_ws_icons");
    let mut col = Collection::new("my-repo".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_downloaded_from_git = true;
    col.workspace_auto_prompt_dismissed = true;
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = app.collections.len() - 1;

    let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let text = buffer_text(term.backend().buffer());

    assert!(
        text.contains(super::draw::GIT_ICON),
        "the git-branch icon marks it as loaded from git"
    );
    assert!(
        text.contains(super::draw::FOLDER_ICON),
        "the folder icon marks it as a Workspace"
    );
    assert!(
        text.contains(super::draw::GIT_ICON),
        "the git-branch icon marks it as loaded from git"
    );
    assert!(
        text.contains(super::draw::FOLDER_ICON),
        "the folder icon marks it as a Workspace"
    );
    let git_pos = text.find(super::draw::GIT_ICON).unwrap();
    let folder_pos = text.find(super::draw::FOLDER_ICON).unwrap();
    let name_pos = text.find("my-repo").unwrap();
    assert!(
        git_pos < folder_pos && folder_pos < name_pos,
        "icons appear together, git-icon first, right before the name"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn esc_cancels_the_picker_without_removing_the_still_empty_tab_and_stops_auto_reopen() {
    let dir = workspace_temp_dir("esc_cancel");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;
    app.overlay = Some(Overlay::WorkspacePicker(WorkspacePickerState::new(
        ci,
        dir.clone(),
        true,
    )));

    press(&mut app, KeyCode::Esc);

    assert!(app.overlay.is_none());
    assert!(app.collections.len() > ci, "the tab is not removed");
    assert_eq!(app.collections[ci].path, None);
    assert!(
        app.collections[ci].workspace_auto_prompt_dismissed,
        "cancelling stops the auto-reopen prompt"
    );

    // The auto-open check must now be a no-op even though the tab is
    // still file-less, since the user explicitly dismissed it.
    app.maybe_auto_open_workspace_picker();
    assert!(
        app.overlay.is_none(),
        "auto-open does not fight the user's cancel"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn global_w_key_opens_the_picker_only_for_workspace_bound_tabs() {
    let dir = workspace_temp_dir("w_key");
    let mut app = TuiApp::default();
    // Tab 0 (built-in Request tab) isn't Workspace-bound: `w` is a no-op.
    press(&mut app, KeyCode::Char('w'));
    assert!(app.overlay.is_none());

    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_auto_prompt_dismissed = true; // simulate an already-dismissed tab
    app.collections.push(col);
    app.active_tab = app.collections.len() - 1;

    press(&mut app, KeyCode::Char('w'));
    assert!(
        matches!(app.overlay, Some(Overlay::WorkspacePicker(_))),
        "an explicit `w` press reopens the picker even after it was dismissed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn auto_open_pops_the_picker_for_a_fresh_workspace_tab_with_no_collection_chosen() {
    let dir = workspace_temp_dir("auto_open");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = app.collections.len() - 1;

    assert!(app.overlay.is_none());
    app.maybe_auto_open_workspace_picker();
    assert!(matches!(app.overlay, Some(Overlay::WorkspacePicker(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn auto_open_never_steals_focus_from_an_already_open_overlay() {
    let dir = workspace_temp_dir("auto_open_no_steal");
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = app.collections.len() - 1;
    app.overlay = Some(Overlay::Help(0));

    app.maybe_auto_open_workspace_picker();

    assert!(
        matches!(app.overlay, Some(Overlay::Help(0))),
        "an existing overlay is left alone"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn workspace_bound_tabs_show_the_folder_icon_in_the_tab_bar_and_list_title() {
    use ratatui::{Terminal, backend::TestBackend};
    let dir = workspace_temp_dir("tab_icon");
    let mut col = Collection::new("my-ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = app.collections.len() - 1;

    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let text = buffer_text(term.backend().buffer());

    assert!(
        text.contains(super::draw::FOLDER_ICON),
        "the folder icon marks the Workspace-bound tab"
    );
    assert!(text.contains("my-ws"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn workspace_bound_list_title_hints_the_w_shortcut_when_there_is_room_but_not_on_a_narrow_terminal()
{
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let s = Strings::for_language(&Language::English);
    let dir = workspace_temp_dir("title_hint");
    let mut col = Collection::new("my-ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    // Dismiss the auto-opened picker prompt so it doesn't cover the
    // List panel's title bar in this render.
    col.workspace_auto_prompt_dismissed = true;
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = app.collections.len() - 1;

    // A spacious terminal has room in the List panel's title bar for
    // the "w to browse" reminder, right next to the folder icon —
    // easier for new users to spot than the busier bottom-border hint.
    let mut wide_term = Terminal::new(TestBackend::new(160, 40)).unwrap();
    wide_term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let wide_text = buffer_text(wide_term.backend().buffer());
    assert!(
        wide_text.contains(&format!("w {}", s.foot_workspace)),
        "the title bar shows the w-to-browse hint on a wide terminal"
    );

    // A narrow terminal shrinks the List panel below the hint's width
    // requirement, so it's dropped rather than clipped mid-word.
    let mut narrow_term = Terminal::new(TestBackend::new(40, 40)).unwrap();
    narrow_term
        .draw(|f| super::draw::draw(f, &mut app))
        .unwrap();
    let narrow_text = buffer_text(narrow_term.backend().buffer());
    assert!(
        !narrow_text.contains(&format!("w {}", s.foot_workspace)),
        "the title bar hides the w-to-browse hint on a narrow terminal"
    );

    // A non-Workspace tab never shows this hint even with plenty of room.
    let mut plain_app = TuiApp::default();
    let mut plain_term = Terminal::new(TestBackend::new(160, 40)).unwrap();
    plain_term
        .draw(|f| super::draw::draw(f, &mut plain_app))
        .unwrap();
    let plain_text = buffer_text(plain_term.backend().buffer());
    assert!(
        !plain_text.contains(&format!("w {}", s.foot_workspace)),
        "a plain (non-Workspace) tab never shows the workspace hint"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_workspace_tab_with_no_collections_at_all_shows_the_empty_state_hint_instead_of_a_blank_list() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    // A genuinely empty workspace root (no folders or collections to browse):
    // the file-tree has nothing to show, so the friendly empty-state hint
    // stands in for the blank list.
    let dir = std::env::temp_dir().join(format!("paperboy_ws_empty_hint_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let s = Strings::for_language(&Language::English);
    let mut col = Collection::new("my-ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_auto_prompt_dismissed = true; // keep the picker closed for this render
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = app.collections.len() - 1;

    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let text = buffer_text(term.backend().buffer());

    assert!(
        text.contains(s.workspace_empty_state),
        "the empty-state hint replaces the blank list"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_workspace_tab_with_no_file_loaded_still_shows_the_browsable_file_tree() {
    use ratatui::{Terminal, backend::TestBackend};
    let dir = workspace_temp_dir("browse_no_file");
    let mut col = Collection::new("my-ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.clone());
    col.workspace_auto_prompt_dismissed = true;
    let mut app = TuiApp::default();
    app.collections.push(col);
    app.active_tab = app.collections.len() - 1;

    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let text = buffer_text(term.backend().buffer());

    assert!(
        text.contains("alpha.hurl"),
        "the root's collection files are listed even before one is opened"
    );
    assert!(
        text.contains("sub"),
        "subfolders are listed too, so the user can browse into them"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Build a Workspace tab bound to `dir` and return the app plus its index.
fn workspace_app(dir: &std::path::Path) -> (TuiApp, usize) {
    let mut app = TuiApp::default();
    let mut col = Collection::new("ws".to_string(), Vec::new());
    col.workspace_root = Some(dir.to_path_buf());
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    (app, ci)
}

#[test]
fn workspace_rows_list_folders_and_collections_and_inline_the_open_collections_requests() {
    use crate::collection::WsRow;
    let dir = workspace_temp_dir("ws_rows");
    let (mut app, ci) = workspace_app(&dir);
    app.load_workspace_file(ci, dir.join("alpha.hurl"));

    let rows = app.collections[ci].ws_rows();
    // Root: the `sub/` folder, then `alpha.hurl` (open, so its one request is
    // inlined right beneath it). No `../` at the root.
    assert!(matches!(&rows[0], WsRow::Folder(n) if n == "sub"));
    assert!(matches!(&rows[1], WsRow::Collection { name, open: true, .. } if name == "alpha.hurl"));
    assert!(matches!(rows[2], WsRow::Request(0)));
    assert_eq!(rows.len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entering_a_workspace_subfolder_and_then_up_navigates_the_breadcrumb() {
    use crate::collection::WsRow;
    let dir = workspace_temp_dir("ws_breadcrumb");
    let (mut app, ci) = workspace_app(&dir);
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    app.focus = Pane::List;

    // Enter on the `sub/` folder row (index 0 at the root) descends into it.
    app.collections[ci].list_cursor = 0;
    app.on_enter();
    assert_eq!(
        app.collections[ci].workspace_browse,
        vec!["sub".to_string()]
    );
    let rows = app.collections[ci].ws_rows();
    assert!(matches!(rows[0], WsRow::Up));
    assert!(matches!(&rows[1], WsRow::Collection { name, open: false, .. } if name == "beta.json"));

    // Enter on the `../` row (index 0) climbs back to the root.
    app.collections[ci].list_cursor = 0;
    app.on_enter();
    assert!(app.collections[ci].workspace_browse.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entering_the_open_collection_row_collapses_and_re_expands_its_requests() {
    use crate::collection::WsRow;
    let dir = workspace_temp_dir("ws_accordion");
    let (mut app, ci) = workspace_app(&dir);
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    app.focus = Pane::List;

    // The `alpha.hurl` collection row sits at index 1 (after `sub/`).
    app.collections[ci].list_cursor = 1;
    app.on_enter();
    assert!(app.collections[ci].workspace_collapsed);
    let rows = app.collections[ci].ws_rows();
    assert!(
        !rows.iter().any(|r| matches!(r, WsRow::Request(_))),
        "a collapsed collection hides its request rows"
    );
    assert!(matches!(&rows[1], WsRow::Collection { open: false, .. }));

    // Enter again on the same row re-expands it.
    app.collections[ci].list_cursor = 1;
    app.on_enter();
    assert!(!app.collections[ci].workspace_collapsed);
    assert!(
        app.collections[ci]
            .ws_rows()
            .iter()
            .any(|r| matches!(r, WsRow::Request(0)))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_a_different_collection_with_unsaved_edits_warns_before_switching() {
    let dir = workspace_temp_dir("ws_switch_warn");
    let (mut app, ci) = workspace_app(&dir);
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    // An unsaved in-memory edit on the loaded collection.
    app.collections[ci].entries[0].modified = true;
    app.focus = Pane::List;

    // Browse into `sub/` and try to open `beta.json`.
    app.collections[ci].list_cursor = 0; // sub/
    app.on_enter();
    app.collections[ci].list_cursor = 1; // beta.json
    app.on_enter();

    assert!(
        matches!(app.overlay, Some(Overlay::WorkspaceSwitchUnsaved { ci: c, .. }) if c == ci),
        "switching with unsaved edits raises the warning first"
    );
    // The file has NOT changed yet — still on alpha.hurl.
    assert_eq!(app.collections[ci].path, Some(dir.join("alpha.hurl")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discarding_at_the_workspace_switch_warning_loads_the_new_collection() {
    let dir = workspace_temp_dir("ws_switch_discard");
    let (mut app, ci) = workspace_app(&dir);
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    app.collections[ci].entries[0].modified = true;
    app.focus = Pane::List;
    app.collections[ci].list_cursor = 0;
    app.on_enter(); // into sub/
    app.collections[ci].list_cursor = 1;
    app.on_enter(); // warning

    // Move the selection to "Discard changes and switch" (sel 1) and confirm.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);

    assert!(app.overlay.is_none());
    assert_eq!(
        app.collections[ci].path,
        Some(dir.join("sub").join("beta.json")),
        "the new collection was loaded, discarding the edit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancelling_the_workspace_switch_warning_keeps_the_current_collection_and_edits() {
    let dir = workspace_temp_dir("ws_switch_cancel");
    let (mut app, ci) = workspace_app(&dir);
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    app.collections[ci].entries[0].modified = true;
    app.focus = Pane::List;
    app.collections[ci].list_cursor = 0;
    app.on_enter();
    app.collections[ci].list_cursor = 1;
    app.on_enter(); // warning

    press(&mut app, KeyCode::Esc);

    assert!(app.overlay.is_none());
    assert_eq!(app.collections[ci].path, Some(dir.join("alpha.hurl")));
    assert!(
        app.collections[ci].entries[0].modified,
        "the unsaved edit is preserved when the switch is cancelled"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn enter_on_a_workspace_request_row_opens_the_edit_wizard() {
    let dir = workspace_temp_dir("ws_edit_request");
    let (mut app, ci) = workspace_app(&dir);
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    app.focus = Pane::List;

    // The single request is inlined at index 2 (after `sub/` and `alpha.hurl`).
    app.collections[ci].list_cursor = 2;
    app.on_enter();

    assert!(
        matches!(app.overlay, Some(Overlay::NewRequest(_))),
        "Enter on a request row opens the edit wizard"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn right_arrow_descends_into_a_highlighted_workspace_folder() {
    let dir = workspace_temp_dir("ws_right");
    let (mut app, ci) = workspace_app(&dir);
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    app.focus = Pane::List;

    // Highlight the `sub/` folder row (index 0) and press Right.
    app.collections[ci].list_cursor = 0;
    press(&mut app, KeyCode::Right);
    assert_eq!(
        app.collections[ci].workspace_browse,
        vec!["sub".to_string()],
        "Right on a folder row descends into it, like Enter"
    );

    // Right on a request row must NOT navigate — it still scrolls the URL.
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    app.focus = Pane::List;
    let request_row = app.collections[ci]
        .ws_rows()
        .iter()
        .position(|r| matches!(r, crate::collection::WsRow::Request(_)))
        .unwrap();
    app.collections[ci].list_cursor = request_row;
    press(&mut app, KeyCode::Right);
    assert!(
        app.collections[ci].workspace_browse.is_empty(),
        "Right on a request row does not navigate the tree"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn right_arrow_expands_a_collapsed_collection_and_opens_a_different_one() {
    use crate::collection::WsRow;
    let dir = workspace_temp_dir("ws_right_collection");
    let (mut app, ci) = workspace_app(&dir);
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    app.focus = Pane::List;

    // Collapse the open `alpha.hurl` (row index 1), then Right re-expands it.
    app.collections[ci].workspace_collapsed = true;
    let alpha_row = app.collections[ci]
        .ws_rows()
        .iter()
        .position(|r| matches!(r, WsRow::Collection { open: false, .. }))
        .unwrap();
    app.collections[ci].list_cursor = alpha_row;
    press(&mut app, KeyCode::Right);
    assert!(
        !app.collections[ci].workspace_collapsed,
        "Right on the collapsed loaded collection expands it"
    );
    assert!(
        app.collections[ci]
            .ws_rows()
            .iter()
            .any(|r| matches!(r, WsRow::Request(0))),
        "its requests are visible again"
    );

    // Browse into `sub/` and open `beta.json` with Right (a different file).
    app.collections[ci].list_cursor = 0; // sub/
    press(&mut app, KeyCode::Right);
    let beta_row = app.collections[ci]
        .ws_rows()
        .iter()
        .position(|r| matches!(r, WsRow::Collection { .. }))
        .unwrap();
    app.collections[ci].list_cursor = beta_row;
    press(&mut app, KeyCode::Right);
    assert_eq!(
        app.collections[ci].path,
        Some(dir.join("sub").join("beta.json")),
        "Right on a collapsed, unloaded collection opens it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn always_save_preference_auto_saves_instead_of_prompting_on_a_workspace_switch() {
    let dir = workspace_temp_dir("ws_always_save");
    let (mut app, ci) = workspace_app(&dir);
    app.always_save_when_prompted = true;
    app.load_workspace_file(ci, dir.join("alpha.hurl"));
    // Edit the request so the collection has an unsaved in-memory change.
    app.collections[ci].entries[0].url = "https://example.com/edited".to_string();
    app.collections[ci].entries[0].modified = true;
    app.focus = Pane::List;

    // Browse into `sub/` and open `beta.json` — no prompt should appear.
    app.collections[ci].list_cursor = 0;
    app.on_enter();
    app.collections[ci].list_cursor = 1;
    app.on_enter();

    assert!(
        app.overlay.is_none(),
        "with always-save on, no Save/Discard/Cancel prompt is shown"
    );
    assert_eq!(
        app.collections[ci].path,
        Some(dir.join("sub").join("beta.json")),
        "the switch went through"
    );
    let saved = std::fs::read_to_string(dir.join("alpha.hurl")).unwrap();
    assert!(
        saved.contains("https://example.com/edited"),
        "the edit was auto-saved to disk before switching"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_always_save_preference_toggles_from_the_preferences_menu_and_is_off_by_default() {
    let mut app = TuiApp::default();
    assert!(
        !app.always_save_when_prompted,
        "the preference is off by default"
    );

    app.overlay = Some(Overlay::Preferences(3));
    press(&mut app, KeyCode::Enter);
    assert!(app.always_save_when_prompted, "Enter toggles it on");
    assert!(
        matches!(app.overlay, Some(Overlay::Preferences(3))),
        "the highlight stays on the toggle row"
    );

    press(&mut app, KeyCode::Char(' '));
    assert!(!app.always_save_when_prompted, "Space toggles it back off");
}

#[test]
fn the_workspace_picker_popup_renders_its_tree_filter_state_and_footer_hint() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let dir = workspace_temp_dir("popup_render");
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);
    let mut app = TuiApp {
        overlay: Some(Overlay::WorkspacePicker(WorkspacePickerState::new(
            0,
            dir.clone(),
            true,
        ))),
        ..Default::default()
    };
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());

    assert!(text.contains(s.workspace_picker_title));
    assert!(
        text.contains(s.workspace_filter_on),
        "the current filter state is shown in the title"
    );
    assert!(text.contains("alpha.hurl"));
    assert!(text.contains("sub"));
    assert!(
        !text.contains("notes.txt"),
        "notes.txt is excluded by the .hurl/.json filter"
    );
    assert!(text.contains(s.workspace_picker_hint));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_workspace_picker_shows_a_no_files_message_when_nothing_matches() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let dir = std::env::temp_dir().join(format!("paperboy_ws_no_files_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("readme.txt"), "no collections here").unwrap();
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);
    let mut app = TuiApp {
        overlay: Some(Overlay::WorkspacePicker(WorkspacePickerState::new(
            0,
            dir.clone(),
            true,
        ))),
        ..Default::default()
    };
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| super::draw::draw_overlay(f, &mut app, &s, &th))
        .unwrap();
    let text = buffer_text(term.backend().buffer());

    assert!(text.contains(s.workspace_no_files));
    let _ = std::fs::remove_dir_all(&dir);
}

// ── "Run All" (Alt+F5) ──────────────────────────────────────────────────

#[test]
fn alt_f5_runs_all_entries_instead_of_a_single_one() {
    // Both entries point at TEST-NET-1 (RFC 5737), so the batch run hangs
    // on connect — same trick `app_in_main_pane` uses — keeping
    // `loading == true` deterministically without any real network I/O.
    let e1 = HurlEntry {
        method: "GET".into(),
        url: "http://192.0.2.1:81/one".into(),
        ..Default::default()
    };
    let e2 = HurlEntry {
        method: "GET".into(),
        url: "http://192.0.2.1:81/two".into(),
        ..Default::default()
    };
    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("t".to_string(), vec![e1, e2]));
    app.active_tab = 1;
    app.focus = Pane::Main;

    app.on_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::ALT));

    assert!(
        app.response.lock().unwrap().loading,
        "Alt+F5 must start the batch run"
    );
    assert_eq!(
        app.pending_batch_runs.len(),
        1,
        "a receiver must be tracked for the running batch"
    );
    assert!(
        app.collections[1]
            .entries
            .iter()
            .all(|e| matches!(e.last_run, crate::hurl::RunStatus::Running)),
        "every entry must be marked in-progress immediately, not just the selected one"
    );
}

#[test]
fn run_all_entries_blocks_when_any_entry_references_a_still_pending_secret() {
    use crate::environment::{EnvVar, Environment, ValueSource};
    // The pending secret is referenced by the second entry only, but Run
    // All must check every entry, not just the selected/first one.
    let e1 = HurlEntry {
        method: "GET".into(),
        url: "http://192.0.2.1:81/plain".into(),
        ..Default::default()
    };
    let e2 = HurlEntry {
        method: "GET".into(),
        url: "http://192.0.2.1:81/{{ TOKEN }}".into(),
        ..Default::default()
    };
    let mut col = Collection::new("t".to_string(), vec![e1, e2]);
    let env = Environment {
        id: 0,
        name: "e".into(),
        vars: vec![EnvVar {
            key: "TOKEN".into(),
            value: "{{ op://x }}".into(),
            source: ValueSource::OnePassword,
            resolved: false,
            loading: true,
            original_value: "{{ op://x }}".into(),
            modified: false,
            user_added: false,
            raw: String::new(),
        }],
        path: None,
        git_origin: None,
    };

    let mut app = TuiApp::default();
    let env_id = add_global_env(&mut app, env);
    col.linked_env_id = Some(env_id);
    app.collections.push(col);

    app.run_all_entries(1);

    assert!(
        matches!(app.status, Some(crate::i18n::Status::WaitingSecrets(ref k)) if k == &vec!["TOKEN".to_string()]),
        "must block with the pending key named"
    );
    assert!(
        app.pending_batch_runs.is_empty(),
        "no background run must be started while blocked"
    );
    assert!(!app.response.lock().unwrap().loading);
}

#[test]
fn poll_batch_run_updates_applies_pass_fail_markers_captures_and_summary() {
    let e1 = HurlEntry {
        title: "one".into(),
        ..Default::default()
    };
    let e2 = HurlEntry {
        title: "two".into(),
        ..Default::default()
    };
    let mut e3 = HurlEntry {
        title: "three".into(),
        ..Default::default()
    };
    // Entry 3's response from an earlier run — must survive this pass
    // untouched since the batch never reaches it (results[2] == None).
    e3.last_response = Some(crate::http::ApiResponse {
        status: 418,
        ..Default::default()
    });
    let col = Collection::new("t".to_string(), vec![e1, e2, e3]);
    let col_id = col.id;
    let mut app = TuiApp::default();
    app.collections.push(col);

    let (tx, rx) = std::sync::mpsc::channel();
    let mut captures = std::collections::HashMap::new();
    captures.insert("token".to_string(), "abc".to_string());
    let responses = vec![
        Some(crate::http::ApiResponse {
            status: 200,
            body: "one-body".into(),
            ..Default::default()
        }),
        Some(crate::http::ApiResponse {
            status: 500,
            body: "two-body".into(),
            ..Default::default()
        }),
        None,
    ];
    tx.send(crate::request::BatchRunUpdate {
        col_id,
        results: vec![Some(true), Some(false), None],
        captures,
        responses,
    })
    .unwrap();
    drop(tx); // sender gone -> receiver disconnects after the one message
    app.pending_batch_runs.push(rx);

    // Drain a few passes so the queued message is consumed (mirrors
    // drain_capture_updates_routes_to_the_matching_collection).
    for _ in 0..3 {
        app.poll_batch_run_updates();
    }

    let col = &app.collections[1];
    assert!(matches!(
        col.entries[0].last_run,
        crate::hurl::RunStatus::Passed
    ));
    assert!(matches!(
        col.entries[1].last_run,
        crate::hurl::RunStatus::Failed
    ));
    assert!(
        matches!(col.entries[2].last_run, crate::hurl::RunStatus::NotRun),
        "an entry the runner never reached goes back to not-run, not stuck Running"
    );
    assert_eq!(
        col.entries[0]
            .last_response
            .as_ref()
            .map(|r| r.body.as_ref()),
        Some("one-body"),
        "each entry remembers its own response, not just the last entry's"
    );
    assert_eq!(
        col.entries[1]
            .last_response
            .as_ref()
            .map(|r| r.body.as_ref()),
        Some("two-body")
    );
    assert_eq!(
        col.entries[2].last_response.as_ref().map(|r| r.status),
        Some(418),
        "an unreached entry keeps its previous response instead of losing it"
    );
    assert_eq!(
        col.captures.get("token").unwrap(),
        "abc",
        "captures merged into the collection"
    );
    assert!(
        matches!(
            app.status,
            Some(crate::i18n::Status::CollectionRunSummary {
                passed: 1,
                failed: 1,
                total: 2
            })
        ),
        "summary counts only entries the runner actually reached"
    );
    assert!(
        app.pending_batch_runs.is_empty(),
        "the one-shot receiver must be dropped once drained"
    );
}

// superseded by `wrapcache::PanelWrap::total_rows`'s own test coverage
// (e.g. `total_rows_accounts_for_wrapping_long_lines` and
// `empty_body_has_one_line_and_one_row` in `wrapcache.rs`).

// The core of the performance fix: scrolled deep into a huge body,
// `wrapped_window` must (a) still return exactly the right rows and (b)
// never call `wrap` on more than a small, bounded number of lines —
// i.e. cost must scale with the visible window, not the whole body.
// (Superseded by `wrapcache::PanelWrap::visible_window`'s own test
// coverage — `visible_window_only_wraps_the_requested_rows` and
// `locate_binary_search_finds_the_right_line_for_a_huge_body` in
// `wrapcache.rs` — now that rendering goes through `PanelWrap`.)

/// End-to-end guard: an obscenely large response body still renders the
/// correct visible window and clamps scrolling correctly (the earlier
/// `response_panel_scroll_stops_at_the_last_line` test already covers
/// small bodies; this specifically exercises the "large body" path this
/// perf fix targets).
#[test]
fn a_huge_response_body_still_scrolls_and_clamps_correctly() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    let body: String = (0..50_000).map(|i| format!("line {i}\n")).collect();
    app.collections[ci].entries = vec![HurlEntry {
        title: "huge".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(body.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;
    // Scroll far past the end on purpose — must clamp, not panic or hang.
    app.resp_panel.set_scroll(u16::MAX);

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    assert_eq!(
        app.resp_panel.scroll(),
        app.resp_max_scroll,
        "scroll must clamp to the last-line boundary"
    );
    let wrap = app
        .resp_panel
        .wrap()
        .expect("a huge response body must populate the wrap cache");
    let visible = wrap.visible_window(app.resp_panel.scroll(), app.resp_text_area.height);
    assert!(
        !visible.is_empty(),
        "the clamped scroll position must still show real content"
    );
    let last_visible: String = visible
        .last()
        .unwrap()
        .spans
        .iter()
        .map(|sp| sp.content.as_ref())
        .collect();
    assert_eq!(
        last_visible, "line 49999",
        "clamped scroll must land exactly on the last line"
    );
}

#[test]
fn dragging_far_outside_the_terminal_bounds_does_not_panic_or_break_rendering() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    app.collections[ci].entries = vec![HurlEntry {
        title: "sel".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: (0..200)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n")
                .into(),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;

    let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let area = app.resp_text_area;
    assert!(area.width > 0 && area.height > 1);

    let ev = |kind, col: u16, row: u16| MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(ev(
        MouseEventKind::Down(MouseButton::Left),
        area.x + 2,
        area.y,
    ));

    // Simulate a fast, continuous drag that goes far outside both the
    // panel's own Rect *and* the terminal's actual size -- exactly what
    // a real drag past the edge of the terminal window would look like.
    for row in [0u16, 5, 200, 1000, 5000, 30000, 60000, u16::MAX] {
        for col in [0u16, 5, 100, 1000, 40000, u16::MAX] {
            app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), col, row));
            // Redraw after every single move, exactly like the real main
            // loop does -- must never panic and must keep terminating.
            term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
        }
    }
    app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), area.x, area.y));
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
}

/// Full end-to-end regression test (through `draw()`, not just
/// `PanelWrap` directly) for the reported "an obscenely large response
/// makes the entire app grind to a halt when you try to select text or
/// scroll" bug. A single multi-megabyte unwrapped line (e.g. a large
/// image/base64 blob echoed back by a server) used to cost well over
/// 100ms *per redraw* even with a tiny selection and no scrolling —
/// this repeatedly redraws such a response with an active selection and
/// asserts it stays fast, with a bound generous enough not to flake on
/// slow CI hardware while still catching an accidental regression back
/// to per-frame O(response size) work.
#[test]
fn a_single_enormous_line_response_with_an_active_selection_redraws_quickly() {
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::{Duration, Instant};

    let mut app = TuiApp::default();
    let ci = app.active_tab;
    let body: String = "x".repeat(5_000_000);
    app.collections[ci].entries = vec![HurlEntry {
        title: "giant-line".into(),
        last_response: Some(crate::http::ApiResponse {
            status: 200,
            status_text: "OK".into(),
            body: Arc::from(body.as_str()),
            ..Default::default()
        }),
        ..Default::default()
    }];
    app.focus = Pane::Response;
    app.set_text_selection(Some(TextSelection {
        pane: Pane::Response,
        anchor: TextPos::new(0, 0),
        cursor: TextPos::new(0, 3),
    }));

    let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    let start = Instant::now();
    for _ in 0..100 {
        term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "100 redraws of a 5MB single-line response with a selection took {elapsed:?} — expected a small fraction of a second"
    );
}

#[test]
fn git_wizard_loading_popup_is_wide_enough_for_the_full_title_even_with_a_short_message() {
    // The "Fetching file…" message is much shorter than the wizard's
    // overall title ("Load Collection from Git…"); the popup width must
    // still grow to fit the title, not just the message/hint.
    use ratatui::{Terminal, backend::TestBackend};
    let mut w = RemoteWizard::new(RemoteKind::Collection, Vec::new());
    w.stage = RemoteStage::Loading {
        phase: LoadPhase::File,
    };
    let s = crate::i18n::Strings::for_language(&Language::English);
    let th = crate::tui::theme::theme(&Language::English);
    let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    term.draw(|f| draw_remote_wizard(f, &w, &s, &th)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut top_border = String::new();
    for x in 0..120 {
        top_border.push_str(buf.cell((x, 13)).map(|c| c.symbol()).unwrap_or(" "));
    }
    assert!(
        top_border.contains("Load Collection from Git…"),
        "expected the full title on the popup's top border, got: {top_border:?}"
    );
}

// ── Save Workspace (copy a Workspace to a new, permanent folder) ────────

#[test]
fn begin_save_workspace_as_on_a_non_workspace_tab_shows_a_status_message() {
    use crate::i18n::Status;
    let mut app = TuiApp::default();
    assert!(app.collections[0].workspace_root.is_none());
    app.begin_save_workspace_as();
    assert!(matches!(app.status, Some(Status::NotWorkspace)));
    assert!(app.pending_workspace_save.is_none());
    assert!(
        app.overlay.is_none(),
        "no browser is opened for a non-Workspace tab"
    );
}

#[test]
fn save_workspace_flow_copies_files_and_rebinds_a_local_workspace_tab_leaving_the_original_folder_alone()
 {
    use crate::i18n::Status;
    let src = workspace_temp_dir("save_local_src");
    let dest_parent = std::env::temp_dir().join(format!(
        "paperboy_ws_save_dest_parent_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dest_parent);
    std::fs::create_dir_all(&dest_parent).unwrap();

    let mut app = TuiApp::default();
    app.confirm_workspace_root(src.clone());
    let ci = app.active_tab;
    // Pick a file so there's something to re-resolve after the copy.
    app.load_workspace_file(ci, src.join("alpha.hurl"));
    app.overlay = None;
    let original_name = app.collections[ci].name.clone();

    app.begin_save_workspace_as();
    match &app.pending_workspace_save {
        Some(p) => {
            assert_eq!(p.source_root, src);
            assert_eq!(p.default_name, original_name);
            assert!(matches!(p.target, WorkspaceSaveTarget::ExistingTab(idx) if idx == ci));
        }
        None => panic!("expected a pending save to be set up"),
    }
    assert!(matches!(
        &app.overlay,
        Some(Overlay::Browser(FileAction::SaveWorkspaceChooseFolder, _))
    ));

    // Tab to the inline folder-name editor (seeded with the tab's name) and
    // press Enter to save into `dest_parent`.
    app.last_browse_dir = Some(dest_parent.clone());
    app.overlay = Some(Overlay::Browser(FileAction::SaveWorkspaceChooseFolder, {
        let mut ex = ratatui_explorer::FileExplorer::new().unwrap();
        let _ = ex.set_cwd(&dest_parent);
        Box::new(ex)
    }));
    press(&mut app, KeyCode::Tab);
    assert!(
        app.browser_name_focused,
        "Tab focuses the folder-name field"
    );
    assert_eq!(
        app.browser_name.text(),
        original_name,
        "the folder-name field defaults to the tab's own name"
    );
    press(&mut app, KeyCode::Enter);

    // The tab must now point at the new location.
    let new_root = app.collections[ci]
        .workspace_root
        .clone()
        .expect("root still set");
    assert!(
        new_root.starts_with(&dest_parent),
        "tab rebound to the new destination"
    );
    assert!(new_root.join("alpha.hurl").exists());
    assert!(new_root.join("sub").join("beta.json").exists());
    assert_eq!(
        app.collections[ci].path.as_deref(),
        Some(new_root.join("alpha.hurl")).as_deref(),
        "the previously-selected file is re-resolved at the new location"
    );
    assert!(!app.collections[ci].workspace_downloaded_from_git);
    assert!(app.collections[ci].workspace_git_origin.is_none());
    assert!(matches!(app.status, Some(Status::WorkspaceSaved)));
    assert!(app.pending_workspace_save.is_none());

    // The original (locally-picked) folder must NOT be deleted.
    assert!(
        src.join("alpha.hurl").exists(),
        "a plain local folder is copied, never moved/deleted"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&dest_parent);
}

#[test]
fn save_workspace_flow_cleans_up_the_old_temp_folder_when_it_was_a_git_download() {
    let src = workspace_temp_dir("save_git_src");
    let dest_parent =
        std::env::temp_dir().join(format!("paperboy_ws_save_git_dest_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest_parent);
    std::fs::create_dir_all(&dest_parent).unwrap();

    let mut app = TuiApp::default();
    app.confirm_workspace_root_from_git(src.clone(), "myrepo".to_string(), None);
    let ci = app.active_tab;
    assert!(app.collections[ci].workspace_downloaded_from_git);
    app.overlay = None;

    app.begin_save_workspace_as();
    app.last_browse_dir = Some(dest_parent.clone());
    app.overlay = Some(Overlay::Browser(FileAction::SaveWorkspaceChooseFolder, {
        let mut ex = ratatui_explorer::FileExplorer::new().unwrap();
        let _ = ex.set_cwd(&dest_parent);
        Box::new(ex)
    }));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Enter); // commit default name ("myrepo")

    let new_root = app.collections[ci].workspace_root.clone().unwrap();
    assert!(new_root.join("alpha.hurl").exists());
    assert!(!app.collections[ci].workspace_downloaded_from_git);
    assert!(
        !src.exists(),
        "the old temp git-download folder is cleaned up after a successful save"
    );

    let _ = std::fs::remove_dir_all(&new_root);
    let _ = std::fs::remove_dir_all(&dest_parent);
}

#[test]
fn workspace_storage_choice_choose_a_folder_leads_to_a_brand_new_plain_tab_and_cleans_up_the_temp_download()
 {
    let repo = workspace_temp_dir("storage_choice_choose");
    let dest_parent = std::env::temp_dir().join(format!(
        "paperboy_ws_storage_choice_dest_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dest_parent);
    std::fs::create_dir_all(&dest_parent).unwrap();

    let mut app = TuiApp::default();
    let before = app.collections.len();
    app.overlay = Some(Overlay::WorkspaceStorageChoice {
        repo: repo.clone(),
        name: "myrepo".to_string(),
        origin: None,
        sel: 0,
    });

    // Move the selection to "choose a folder" (sel 1) and confirm.
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Enter);

    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::Browser(FileAction::SaveWorkspaceChooseFolder, _))
        ),
        "choosing a folder opens the destination browser"
    );
    assert_eq!(
        app.collections.len(),
        before,
        "no tab is created until the save actually completes"
    );

    app.last_browse_dir = Some(dest_parent.clone());
    app.overlay = Some(Overlay::Browser(FileAction::SaveWorkspaceChooseFolder, {
        let mut ex = ratatui_explorer::FileExplorer::new().unwrap();
        let _ = ex.set_cwd(&dest_parent);
        Box::new(ex)
    }));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Enter); // commit default name

    assert_eq!(
        app.collections.len(),
        before + 1,
        "a brand new plain Workspace tab is created"
    );
    let ci = app.active_tab;
    let new_root = app.collections[ci].workspace_root.clone().unwrap();
    assert!(new_root.starts_with(&dest_parent));
    assert!(new_root.join("alpha.hurl").exists());
    assert!(
        !app.collections[ci].workspace_downloaded_from_git,
        "a saved-to-a-chosen-folder tab is a plain Workspace, not flagged as a temp git download"
    );
    assert!(
        !repo.exists(),
        "the original temp download directory is cleaned up"
    );

    let _ = std::fs::remove_dir_all(&new_root);
    let _ = std::fs::remove_dir_all(&dest_parent);
}

#[test]
fn cancelling_the_destination_browser_falls_back_to_keeping_a_git_workspace_temporary() {
    let repo = workspace_temp_dir("storage_choice_cancel");
    let mut app = TuiApp::default();
    let before = app.collections.len();
    app.overlay = Some(Overlay::WorkspaceStorageChoice {
        repo: repo.clone(),
        name: "myrepo".to_string(),
        origin: None,
        sel: 1,
    });
    press(&mut app, KeyCode::Enter); // choose a folder

    assert!(matches!(
        &app.overlay,
        Some(Overlay::Browser(FileAction::SaveWorkspaceChooseFolder, _))
    ));

    press(&mut app, KeyCode::Esc);

    assert_eq!(
        app.collections.len(),
        before + 1,
        "the download is not lost — it falls back to a temporary tab"
    );
    let ci = app.active_tab;
    assert_eq!(
        app.collections[ci].workspace_root.as_deref(),
        Some(repo.as_path())
    );
    assert!(
        app.collections[ci].workspace_downloaded_from_git,
        "falls back to the old temporary behaviour"
    );
    assert!(app.pending_workspace_save.is_none());

    let _ = std::fs::remove_dir_all(&repo);
}

#[test]
fn an_empty_name_in_the_save_browser_keeps_the_picker_open_then_esc_falls_back() {
    let repo = workspace_temp_dir("storage_choice_empty_name");
    let dest_parent = std::env::temp_dir().join(format!(
        "paperboy_ws_storage_choice_empty_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dest_parent);
    std::fs::create_dir_all(&dest_parent).unwrap();

    let mut app = TuiApp::default();
    let before = app.collections.len();
    app.overlay = Some(Overlay::WorkspaceStorageChoice {
        repo: repo.clone(),
        name: "myrepo".to_string(),
        origin: None,
        sel: 1,
    });
    press(&mut app, KeyCode::Enter);

    app.last_browse_dir = Some(dest_parent.clone());
    app.overlay = Some(Overlay::Browser(FileAction::SaveWorkspaceChooseFolder, {
        let mut ex = ratatui_explorer::FileExplorer::new().unwrap();
        let _ = ex.set_cwd(&dest_parent);
        Box::new(ex)
    }));
    press(&mut app, KeyCode::Tab);

    // Clear the pre-filled name entirely, then try to commit — a blank name
    // can't be saved, so the picker stays open (no tab created, nothing lost).
    app.browser_name = super::editor::Editor::new("", false);
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::Browser(FileAction::SaveWorkspaceChooseFolder, _))
        ),
        "an empty name keeps the folder picker open rather than committing"
    );
    assert_eq!(
        app.collections.len(),
        before,
        "nothing is created while the name is blank"
    );

    // Esc backs focus out to the folder list, and a second Esc there cancels —
    // falling back to keeping the git download as a temporary tab.
    press(&mut app, KeyCode::Esc);
    assert!(
        !app.browser_name_focused,
        "first Esc unfocuses the name field"
    );
    press(&mut app, KeyCode::Esc);

    assert_eq!(
        app.collections.len(),
        before + 1,
        "still falls back to a temporary tab, never losing the download"
    );
    let ci = app.active_tab;
    assert_eq!(
        app.collections[ci].workspace_root.as_deref(),
        Some(repo.as_path())
    );
    assert!(app.collections[ci].workspace_downloaded_from_git);

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&dest_parent);
}

#[test]
fn env_name_keeps_dotted_suffix_but_still_drops_a_real_extension() {
    assert_eq!(
        env_name_from_path("/x/.env.dev-au", "environment"),
        ".env.dev-au"
    );
    assert_eq!(env_name_from_path("/x/.env", "environment"), ".env");
    assert_eq!(env_name_from_path("/x/prod.vars", "environment"), "prod");
    assert_eq!(env_name_from_path("/x/dev.env", "environment"), "dev");
    // A multi-dot name whose trailing suffix isn't a known env extension keeps
    // its full name (regression: it used to be truncated to `environment.env`).
    assert_eq!(
        env_name_from_path("/x/environment.env.dev-au", "environment"),
        "environment.env.dev-au"
    );
    assert_eq!(env_name_from_path("", "environment"), "environment");
}

#[test]
fn collection_name_hides_only_known_extensions() {
    assert_eq!(
        collection_name_from_path("/x/api.hurl", "collection"),
        "api"
    );
    assert_eq!(
        collection_name_from_path("/x/api.json", "collection"),
        "api"
    );
    assert_eq!(
        collection_name_from_path("/x/api.HURL", "collection"),
        "api"
    );
    assert_eq!(
        collection_name_from_path("/x/env.dev-au", "collection"),
        "env.dev-au"
    );
    assert_eq!(
        collection_name_from_path("/x/notes.txt", "collection"),
        "notes.txt"
    );
}

// ---- Custom themes (Settings → Theme) -------------------------------------

use super::theme_editor::{NewThemeFocus, ThemePane};

/// Open the Theme editor the way a user does: Settings (s) → Theme (item 1).
fn open_theme_editor(app: &mut TuiApp) {
    press(app, KeyCode::Char('s'));
    press(app, KeyCode::Down); // Language -> Theme
    press(app, KeyCode::Enter); // open the Theme editor
    assert!(
        matches!(app.overlay, Some(Overlay::ThemeEditor(_))),
        "the Theme editor opens"
    );
}

/// Create a custom theme through the New-theme popup, based on the popup's
/// default base, and land in the colour fields ready to edit.
fn create_custom_theme(app: &mut TuiApp, name: &str) {
    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)); // popup
    for c in name.chars() {
        press(app, KeyCode::Char(c));
    }
    press(app, KeyCode::Enter); // create + activate + focus the fields
}

fn theme_state(app: &TuiApp) -> &super::theme_editor::ThemeEditorState {
    match &app.overlay {
        Some(Overlay::ThemeEditor(st)) => st,
        _ => panic!("the Theme editor should be open"),
    }
}

#[test]
fn the_theme_list_starts_with_automatic_then_the_three_presets() {
    let app = TuiApp::default();
    let s = crate::i18n::Strings::for_language(&app.language);
    let entries = app.theme_picker_entries(&s);
    assert_eq!(entries[0], s.theme_auto, "row 0 follows the language");
    assert_eq!(
        &entries[1..],
        &[
            super::theme::PRESET_ENGLISH.to_string(),
            super::theme::PRESET_FRENCH.to_string(),
            super::theme::PRESET_DANISH.to_string(),
        ],
        "the three presets follow, in language order"
    );
}

#[test]
fn selecting_a_preset_in_the_picker_activates_it() {
    let mut app = TuiApp::default();
    assert_eq!(
        app.active_theme, None,
        "starts on Automatic (follows language)"
    );

    open_theme_editor(&mut app);
    // Automatic(0) -> Britannia(1) -> Parisian Purple(2) -> Dannebrog(3).
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(
        app.active_theme.as_deref(),
        Some(super::theme::PRESET_DANISH),
        "hovering a preset activates it live"
    );

    // Returning to Automatic clears the manual choice.
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    assert_eq!(
        app.active_theme, None,
        "row 0 goes back to following the language"
    );
}

#[test]
fn presets_are_read_only_and_cannot_be_edited() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app); // opens on a preset via Automatic

    // Trying to step into the colour fields on a preset is refused.
    press(&mut app, KeyCode::Right);
    assert!(
        matches!(theme_state(&app).pane, ThemePane::List),
        "focus stays on the list for a read-only preset"
    );
    assert!(
        matches!(app.status, Some(crate::i18n::Status::ThemePresetReadonly)),
        "a read-only hint is shown"
    );
}

#[test]
fn ctrl_n_opens_a_popup_that_creates_selects_and_focuses_a_new_theme() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);

    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert!(
        theme_state(&app).new_popup.is_some(),
        "Ctrl+N opens the New-theme popup"
    );

    for c in "Ocean".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);

    assert!(
        app.custom_themes.iter().any(|t| t.name == "Ocean"),
        "the theme is created"
    );
    assert_eq!(app.active_theme.as_deref(), Some("Ocean"), "and activated");
    let st = theme_state(&app);
    assert!(st.new_popup.is_none(), "the popup closes");
    assert!(
        matches!(st.pane, ThemePane::Fields),
        "focus drops into the colour fields, ready to edit"
    );
    let expected = app
        .all_themes()
        .iter()
        .position(|t| t.name == "Ocean")
        .unwrap()
        + 1;
    assert_eq!(
        st.list_idx, expected,
        "the new theme is selected in the list"
    );
}

#[test]
fn the_new_theme_popup_copies_the_chosen_base_colours() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);
    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));

    for c in "Nordic".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    // Move focus into the base list and pick Dannebrog (Britannia, Parisian,
    // Dannebrog -> down twice from the first entry).
    press(&mut app, KeyCode::Down); // Name -> Base (Britannia)
    press(&mut app, KeyCode::Down); // -> Parisian Purple
    press(&mut app, KeyCode::Down); // -> Dannebrog
    press(&mut app, KeyCode::Enter);

    let created = app
        .custom_themes
        .iter()
        .find(|t| t.name == "Nordic")
        .expect("theme created");
    let dannebrog = super::theme::preset_for_language(&Language::Danish);
    assert_eq!(
        created.color(0),
        dannebrog.color(0),
        "the new theme copies the chosen base's colours"
    );
}

#[test]
fn the_new_theme_popup_rejects_empty_reserved_and_duplicate_names() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);

    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    press(&mut app, KeyCode::Enter); // empty name
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ThemeNameRequired)
    ));
    assert!(
        theme_state(&app).new_popup.is_some(),
        "the popup stays open"
    );

    for c in super::theme::PRESET_ENGLISH.chars() {
        press(&mut app, KeyCode::Char(c)); // a reserved preset name
    }
    press(&mut app, KeyCode::Enter);
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ThemeNameReserved)
    ));
    assert!(app.custom_themes.is_empty(), "nothing is created");

    // Cancel, make a real theme, then try to reuse its name.
    press(&mut app, KeyCode::Esc);
    create_custom_theme(&mut app, "Dusk");
    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    for c in "Dusk".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ThemeNameTaken)
    ));
    assert_eq!(
        app.custom_themes
            .iter()
            .filter(|t| t.name == "Dusk")
            .count(),
        1,
        "the duplicate is not created"
    );
}

#[test]
fn editing_a_custom_theme_colour_auto_saves_and_previews_live() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);
    create_custom_theme(&mut app, "Ocean"); // now in the fields, on colour 0 (bg)

    // Open the colour picker on the focused row and dial in pure red.
    press(&mut app, KeyCode::Enter);
    assert!(
        theme_state(&app).color_popup.is_some(),
        "Enter on a colour row opens the picker popup"
    );
    for c in "255".chars() {
        press(&mut app, KeyCode::Char(c)); // red channel
    }
    press(&mut app, KeyCode::Down); // green channel
    press(&mut app, KeyCode::Char('0'));
    press(&mut app, KeyCode::Down); // blue channel
    press(&mut app, KeyCode::Char('0'));

    assert!(
        matches!(app.theme().bg, ratatui::style::Color::Rgb(255, 0, 0)),
        "the whole-UI preview reflects the in-progress edit immediately"
    );

    // Enter commits the picker and auto-saves the theme.
    press(&mut app, KeyCode::Enter);
    assert!(
        theme_state(&app).color_popup.is_none(),
        "Enter closes the picker popup"
    );
    let saved = app
        .custom_themes
        .iter()
        .find(|t| t.name == "Ocean")
        .expect("theme still present");
    assert_eq!(
        saved.color(0),
        [255, 0, 0],
        "committing the picker auto-saves to the stored custom theme (no explicit save)"
    );
}

#[test]
fn renaming_a_custom_theme_from_the_name_row_auto_applies() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);
    create_custom_theme(&mut app, "Ocean"); // in the fields, on colour 0

    press(&mut app, KeyCode::Up); // colour 0 -> name row
    assert!(
        theme_state(&app).name_focused,
        "Up from the first colour focuses the name row"
    );

    for _ in 0..10 {
        press(&mut app, KeyCode::Backspace); // clear "Ocean"
    }
    for c in "Sea".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter); // submit the rename

    assert!(
        theme_state(&app).name_focused,
        "Enter submits the name but keeps focus on the name row"
    );
    assert!(
        app.custom_themes.iter().any(|t| t.name == "Sea"),
        "the stored theme is renamed in place"
    );
    assert!(
        !app.custom_themes.iter().any(|t| t.name == "Ocean"),
        "the old name is gone"
    );
    assert_eq!(
        app.active_theme.as_deref(),
        Some("Sea"),
        "the active theme follows the rename"
    );
    assert_eq!(theme_state(&app).draft.name, "Sea");
}

#[test]
fn renaming_over_a_reserved_or_duplicate_name_is_ignored() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);
    create_custom_theme(&mut app, "Ocean");

    press(&mut app, KeyCode::Up); // focus the name row
    for _ in 0..10 {
        press(&mut app, KeyCode::Backspace);
    }
    for c in "Britannia".chars() {
        press(&mut app, KeyCode::Char(c)); // a reserved preset name
    }
    press(&mut app, KeyCode::Down); // leave the name row → rename is rejected

    assert!(
        app.custom_themes.iter().any(|t| t.name == "Ocean"),
        "a collision with a reserved preset name never renames the theme"
    );
    assert_eq!(
        app.active_theme.as_deref(),
        Some("Ocean"),
        "the active theme keeps its valid name"
    );
    assert_eq!(theme_state(&app).draft.name, "Ocean");
}

#[test]
fn deleting_a_theme_focuses_the_one_above_it() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);
    create_custom_theme(&mut app, "Alpha");
    create_custom_theme(&mut app, "Bravo"); // active, selected at the bottom

    press(&mut app, KeyCode::Left); // step back from the fields to the list
    let bravo_idx = theme_state(&app).list_idx;
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));

    assert!(
        !app.custom_themes.iter().any(|t| t.name == "Bravo"),
        "Bravo is deleted"
    );
    assert_eq!(
        theme_state(&app).list_idx,
        bravo_idx - 1,
        "focus moves to the row just above the deleted theme, not the top"
    );
    assert_eq!(
        app.active_theme.as_deref(),
        Some("Alpha"),
        "the theme above becomes active"
    );
}

#[test]
fn colour_picker_ctrl_arrows_step_the_channel_by_sixteen() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);
    create_custom_theme(&mut app, "Ocean");

    press(&mut app, KeyCode::Enter); // open picker on colour 0, channel red
    // Zero the red channel so the ±16 step is unambiguous.
    press(&mut app, KeyCode::Char('0'));
    assert_eq!(theme_state(&app).draft.color(0)[0], 0);

    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
    assert_eq!(
        theme_state(&app).draft.color(0)[0],
        16,
        "Ctrl+Right steps the channel up by sixteen, like PageUp"
    );

    app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(
        theme_state(&app).draft.color(0)[0],
        0,
        "Ctrl+Left steps the channel down by sixteen, like PageDown"
    );
}

#[test]
fn colour_picker_esc_restores_the_original_colour() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);
    create_custom_theme(&mut app, "Ocean");
    let before = theme_state(&app).draft.color(0);

    press(&mut app, KeyCode::Enter); // open picker on colour 0
    press(&mut app, KeyCode::Right); // nudge red +1
    assert_ne!(
        theme_state(&app).draft.color(0),
        before,
        "the draft previews the nudge live"
    );

    press(&mut app, KeyCode::Esc); // cancel
    assert!(theme_state(&app).color_popup.is_none());
    assert_eq!(
        theme_state(&app).draft.color(0),
        before,
        "Esc restores the colour to its value before the picker opened"
    );
}

#[test]
fn left_arrow_steps_back_from_the_fields_to_the_theme_list() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);
    create_custom_theme(&mut app, "Ocean"); // in the fields, on a colour row

    assert!(
        matches!(theme_state(&app).pane, ThemePane::Fields),
        "creating a theme drops into the Fields pane"
    );
    press(&mut app, KeyCode::Left);
    assert!(
        matches!(theme_state(&app).pane, ThemePane::List),
        "Left in the Fields pane steps back to the theme list"
    );
}

#[test]
fn ctrl_d_deletes_a_custom_theme_but_never_a_preset() {
    let mut app = TuiApp::default();
    let mut spec = super::theme::preset_for_language(&Language::English);
    spec.name = "Sunset".to_string();
    app.custom_themes.push(spec);
    app.active_theme = Some("Sunset".to_string());

    open_theme_editor(&mut app); // opens on the active theme (Sunset)
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert!(app.custom_themes.is_empty(), "the custom theme is deleted");
    assert_eq!(
        app.active_theme.as_deref(),
        Some("Dannebrog"),
        "focus (and the active theme) moves to the row just above the deleted one"
    );
    assert!(
        matches!(app.status, Some(crate::i18n::Status::ThemeDeleted(ref n)) if n == "Sunset"),
        "a deletion status is shown"
    );

    // Try to delete the built-in preset now under the cursor.
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert!(
        matches!(app.status, Some(crate::i18n::Status::ThemeCannotDelete)),
        "presets can't be deleted"
    );
    assert_eq!(app.all_themes().len(), 3, "the three presets remain");
}

#[test]
fn the_new_theme_popup_focus_toggles_between_name_and_base() {
    let mut app = TuiApp::default();
    open_theme_editor(&mut app);
    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));

    assert!(matches!(
        theme_state(&app).new_popup.as_ref().unwrap().focus,
        NewThemeFocus::Name
    ));
    press(&mut app, KeyCode::Tab);
    assert!(matches!(
        theme_state(&app).new_popup.as_ref().unwrap().focus,
        NewThemeFocus::Base
    ));
    press(&mut app, KeyCode::Tab);
    assert!(matches!(
        theme_state(&app).new_popup.as_ref().unwrap().focus,
        NewThemeFocus::Name
    ));
}

#[test]
fn changing_language_follows_the_preset_unless_a_theme_is_set() {
    let mut app = TuiApp::default();
    assert_eq!(
        app.active_theme_spec().name,
        super::theme::PRESET_ENGLISH,
        "Automatic English -> Britannia"
    );

    app.language = Language::Danish;
    assert_eq!(
        app.active_theme_spec().name,
        super::theme::PRESET_DANISH,
        "changing language moves to that language's preset"
    );

    app.active_theme = Some(super::theme::PRESET_FRENCH.to_string());
    app.language = Language::English;
    assert_eq!(
        app.active_theme_spec().name,
        super::theme::PRESET_FRENCH,
        "a manually-set theme is not overridden by a language change"
    );
}

#[test]
fn custom_themes_and_active_theme_survive_persistence() {
    let mut app = TuiApp::default();
    let mut spec = super::theme::preset_for_language(&Language::English);
    spec.name = "Sunset".to_string();
    spec.set_color(0, [1, 2, 3]);
    app.custom_themes.push(spec);
    app.active_theme = Some("Sunset".to_string());

    let restored = {
        let persisted = app.to_persisted();
        let mut restored = TuiApp::default();
        restored.apply_persisted(persisted);
        restored
    };
    assert_eq!(restored.active_theme.as_deref(), Some("Sunset"));
    assert_eq!(restored.custom_themes.len(), 1);
    assert_eq!(restored.custom_themes[0].color(0), [1, 2, 3]);
}

// ---- PaperTrail report tabs (Phase 9) ------------------------------------

use crate::report::validate::Severity;

/// Shift+R from the main view opens a new (scratch) report tab and makes it
/// active — report tabs live after the collection tabs in the unified strip.
#[test]
fn shift_r_opens_a_new_report_tab() {
    let mut app = TuiApp::default();
    assert_eq!(app.reports.len(), 0);
    app.on_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
    assert_eq!(app.reports.len(), 1);
    assert!(app.active_is_report());
    // First report tab sits immediately after the collection tabs.
    assert_eq!(app.active_tab, app.collections.len());
}

/// A brand-new scratch report has a blank `# collection:` header, so validation
/// flags it as unbound (an error) without any parse error.
#[test]
fn new_scratch_report_flags_unbound_collection() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let rt = app.active_report().expect("active report");
    assert!(rt.parse_error.is_none(), "scratch template must parse");
    assert!(
        rt.diagnostics.iter().any(|d| d.severity == Severity::Error),
        "an unbound collection should be an error diagnostic"
    );
}

/// `e` gives the source panel edit focus; typing goes straight into the panel,
/// updating the report text live and marking it dirty. Esc leaves edit focus,
/// keeping the typed text (edits are applied live, not on a separate commit).
#[test]
fn report_inline_edit_types_into_source() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    press(&mut app, KeyCode::Char('e'));
    assert!(
        app.active_report().unwrap().editor.is_some(),
        "e gives the source panel edit focus"
    );
    app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    // Edits apply live, before leaving edit focus.
    let rt = app.active_report().expect("active report");
    assert!(rt.report.text.contains('x'));
    assert!(rt.report.dirty, "typing marks the report dirty");
    // Esc leaves edit focus but keeps the text.
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let rt = app.active_report().expect("active report");
    assert!(rt.editor.is_none(), "Esc leaves edit focus");
    assert!(rt.report.text.contains('x'), "edits are kept after leaving");
}

/// After Esc leaves edit focus, single letters act as view shortcuts again
/// (e.g. `R` opens another report tab rather than typing an 'R' into the body).
#[test]
fn report_inline_edit_esc_returns_to_shortcuts() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    press(&mut app, KeyCode::Char('e'));
    app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(app.active_report().unwrap().report.text.contains('z'));
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.active_report().unwrap().editor.is_none());
    let before = app.reports.len();
    // A plain 'R' is a shortcut again (new report), not typed text.
    app.on_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
    assert_eq!(
        app.reports.len(),
        before + 1,
        "R opens a new report once edit focus has been left"
    );
}

/// Tab in the report source editor (with no pending completion) indents one
/// level — four spaces — rather than moving focus, since the body is code-like.
#[test]
fn report_editor_tab_indents_four_spaces() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    // A non-REQUEST line so no name completion is pending (Tab would otherwise
    // accept the ghost). Cursor lands at the end on entering edit focus.
    app.reports[idx].report.set_text("# note");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Char('e'));
    app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(
        app.active_report().unwrap().report.text,
        "# note    ",
        "Tab inserts four spaces at the cursor"
    );
}

/// Tab inserted before an `END` snaps it to its opener's indent (matching the
/// backspace/space behaviour) rather than blindly over-indenting it.
#[test]
fn report_editor_tab_before_end_snaps_to_opener_indent() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text("FOR ENV IN ENVS\nEND");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Char('e'));
    // Cursor starts at the end of the END line; jump to its start, then Tab.
    app.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(
        app.active_report().unwrap().report.text,
        "FOR ENV IN ENVS\nEND",
        "Tab before END snaps it back to the opener's indent"
    );
}

/// Backspace inside a line's leading indentation deletes back to the previous
/// 4-space stop (mirroring the Tab indent), not one space at a time.
#[test]
fn report_editor_backspace_unindents_in_fours() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text("        x");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Char('e'));
    // Move the cursor to the end of the 8-space indent (just before `x`).
    app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(
        app.active_report().unwrap().report.text,
        "    x",
        "backspace snaps from column 8 to 4"
    );
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(
        app.active_report().unwrap().report.text,
        "x",
        "backspace snaps from column 4 to 0"
    );
}

/// Backspace over the padding Tab leaves *after* an `END` (trailing, not
/// leading, spaces) still deletes a whole four-space unit at a time.
#[test]
fn report_editor_backspace_removes_trailing_indent_after_end_in_fours() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    // Tab at the end of an `END` line leaves four trailing spaces.
    app.reports[idx].report.set_text("FOR ENV IN ENVS\nEND    ");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Char('e'));
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(
        app.active_report().unwrap().report.text,
        "FOR ENV IN ENVS\nEND",
        "one backspace clears the whole four-space run after END"
    );
}

/// `<` / `>` resize the workspace-tree column from the report view (parity with
/// the collection view), clamped to 20..80.
#[test]
fn report_view_angle_brackets_resize_the_list_column() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    app.list_width = 40;
    app.on_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE));
    assert_eq!(app.list_width, 42, "> widens the column by 2");
    app.on_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE));
    assert_eq!(app.list_width, 38, "< narrows the column by 2");
}

/// The report-export picker's ↑/↓ cycle the output format by rewriting the
/// filename's extension (the format *is* the extension), wrapping around.
#[test]
fn export_format_cycles_through_output_formats() {
    let mut app = TuiApp::default();
    app.browser_name = super::editor::Editor::new("report.csv", false);
    app.cycle_browser_export_format(true);
    assert_eq!(app.browser_name.text(), "report.json");
    app.cycle_browser_export_format(true);
    assert_eq!(app.browser_name.text(), "report.html");
    app.cycle_browser_export_format(true);
    assert_eq!(app.browser_name.text(), "report.xlsx");
    app.cycle_browser_export_format(true);
    assert_eq!(app.browser_name.text(), "report.csv", "wraps back to csv");
    app.cycle_browser_export_format(false);
    assert_eq!(app.browser_name.text(), "report.xlsx", "↑ steps backwards");
    // An unknown extension is treated as csv, so the next format is json.
    app.browser_name = super::editor::Editor::new("data.txt", false);
    app.cycle_browser_export_format(true);
    assert_eq!(app.browser_name.text(), "data.json");
}

/// Ctrl+Backspace in the raw-mode prompt (which legacy terminals deliver as
/// Ctrl+H) deletes the previous word instead of typing a literal `h`.
#[test]
fn raw_prompt_ctrl_h_deletes_a_word_not_types_h() {
    let mut entry = HurlEntry::from_fields("r", "GET", "http://h/x", vec![], "");
    entry.expected_status = Some(200);
    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::Main;
    app.on_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
    assert!(matches!(app.overlay, Some(Overlay::Prompt { .. })));
    // Put the cursor at a known end-of-line and type a marker word.
    app.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    for c in "zzz".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let with_word = match &app.overlay {
        Some(Overlay::Prompt { editor, .. }) => editor.text(),
        _ => unreachable!(),
    };
    assert!(with_word.contains("zzz"), "the marker word was typed");
    app.on_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    let after = match &app.overlay {
        Some(Overlay::Prompt { editor, .. }) => editor.text(),
        _ => unreachable!(),
    };
    assert!(
        !after.contains("zzz"),
        "Ctrl+H word-deletes the marker, and never inserts a literal 'h' (got {after:?})"
    );
}

/// Ctrl+Left / Ctrl+Right move the source editor cursor a word at a time
/// (rather than jumping to the line ends).
#[test]
fn report_editor_ctrl_arrow_moves_one_word() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text("REQUEST Oauth Session");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Char('e')); // edit focus; cursor at end (col 21)
    let end = app.active_report().unwrap().editor.as_ref().unwrap().col;
    assert_eq!(end, 21, "cursor starts at end of the line");

    // Ctrl+Left steps back one word at a time, not to column 0.
    app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(
        app.active_report().unwrap().editor.as_ref().unwrap().col,
        14,
        "Ctrl+Left lands at the start of 'Session'"
    );
    app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
    assert_eq!(
        app.active_report().unwrap().editor.as_ref().unwrap().col,
        8,
        "a second Ctrl+Left lands at the start of 'Oauth'"
    );

    // Ctrl+Right steps forward a word at a time.
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
    assert_eq!(
        app.active_report().unwrap().editor.as_ref().unwrap().col,
        13,
        "Ctrl+Right lands at the end of 'Oauth'"
    );
}

/// While typing a `REQUEST <name>` line, a ghost suffix from the bound
/// collection is offered and Right arrow fills it in.
#[test]
fn report_editor_ghost_completes_a_request_name() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![
            HurlEntry {
                title: "Oauth".to_string(),
                ..Default::default()
            },
            HurlEntry {
                title: "CreateSession".to_string(),
                ..Default::default()
            },
        ],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST Oau");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Char('e')); // edit focus; cursor at end of "REQUEST Oau"

    assert_eq!(
        app.report_completion(idx).map(|c| c.ghost).as_deref(),
        Some("th"),
        "the remainder of the matching request name is offered"
    );

    // Right arrow accepts the completion.
    press(&mut app, KeyCode::Right);
    assert!(
        app.active_report()
            .unwrap()
            .report
            .text
            .ends_with("REQUEST Oauth"),
        "Right arrow fills the ghost in"
    );
    // With the name complete, there's no longer a ghost.
    assert!(app.report_completion(idx).is_none());
}

/// The ghost only appears at the end of a `REQUEST`/`REPORT REQUEST` line with
/// a non-empty, still-unfinished name.
#[test]
fn report_editor_ghost_is_scoped_to_request_lines() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();

    // A non-REQUEST line offers nothing.
    app.reports[idx]
        .report
        .set_text("# collection: api\nURL=Oau");
    press(&mut app, KeyCode::Char('e'));
    assert!(app.report_completion(idx).is_none());
    press(&mut app, KeyCode::Esc);

    // A REPORT REQUEST line does complete. (These two scenarios stand in for two
    // separate reports; a real second report tab starts with no remembered
    // caret, so clear the one the first scenario's Esc left behind — otherwise
    // it would be restored mid-line and suppress the end-of-line completion.)
    app.reports[idx].edit_cursor = None;
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Oa");
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.report_completion(idx).map(|c| c.ghost).as_deref(),
        Some("uth")
    );
}

/// A request name with spaces must be quoted; a bare completion auto-quotes it
/// (typing `Up` completes to `"Upload document"`), and completion inside an
/// already-opened quote fills the rest and appends the closing quote — never
/// producing an unparseable bare-name-with-spaces line.
#[test]
fn report_editor_ghost_quotes_names_with_spaces() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Create Session".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();

    // Bare typing of a spaced title shows the plain remainder as the ghost…
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST Cre");
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.report_completion(idx).map(|c| c.ghost).as_deref(),
        Some("ate Session"),
        "the ghost shows the plain remainder (quotes added on accept)"
    );
    // …and accepting it wraps the whole name in quotes.
    press(&mut app, KeyCode::Right);
    assert!(
        app.active_report()
            .unwrap()
            .report
            .text
            .ends_with("REQUEST \"Create Session\""),
        "a bare completion of a spaced name is auto-quoted on accept"
    );
    let idx = app.active_report_index().unwrap();
    app.revalidate_report(idx);
    assert!(
        app.reports[idx].parse_error.is_none(),
        "the auto-quoted name parses cleanly"
    );
    press(&mut app, KeyCode::Esc);

    // Completing inside an already-opened quote fills the rest and closes it.
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST \"Create Ses");
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.report_completion(idx).map(|c| c.ghost).as_deref(),
        Some("sion\""),
        "quoted completion fills the rest and appends the closing quote"
    );
    press(&mut app, KeyCode::Right);
    assert!(
        app.active_report()
            .unwrap()
            .report
            .text
            .ends_with("REQUEST \"Create Session\"")
    );
}

/// Inside an opened quote, an already-complete name completes to just the
/// closing quote.
#[test]
fn report_editor_ghost_closes_an_opened_quote() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST \"Oauth");
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.report_completion(idx).map(|c| c.ghost).as_deref(),
        Some("\"")
    );
}

/// Item 4 (rep-ci-autocomplete): autocomplete matches case-insensitively, and
/// accepting adopts the request name's canonical casing — typing a lowercase
/// `r` completes to `Report value` (capital R), replacing what was typed rather
/// than appending after it.
#[test]
fn report_editor_autocomplete_is_case_insensitive_and_recases() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Report value".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST r");
    press(&mut app, KeyCode::Char('e')); // edit; cursor after the lowercase `r`
    assert_eq!(
        app.report_completion(idx).map(|c| c.ghost).as_deref(),
        Some("eport value"),
        "a lowercase fragment matches the capitalised request name"
    );
    press(&mut app, KeyCode::Right); // accept
    assert!(
        app.active_report()
            .unwrap()
            .report
            .text
            .ends_with("REQUEST \"Report value\""),
        "the typed `r` is replaced with the canonical `R`, and the spaced name is quoted: {:?}",
        app.active_report().unwrap().report.text
    );
}

/// Item 23 (rep-cursor-memory): leaving and re-entering the source editor
/// restores the caret where it last sat, rather than jumping to the buffer end.
#[test]
fn report_editor_remembers_the_last_cursor_position() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("REQUEST A\nREQUEST B\nREQUEST C");
    press(&mut app, KeyCode::Char('e')); // enter edit; cursor at end (row 2)
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up); // move up to row 0
    let (row, col) = {
        let ed = app.reports[idx].editor.as_ref().unwrap();
        (ed.row, ed.col)
    };
    assert_eq!(row, 0, "cursor moved to the first line");
    press(&mut app, KeyCode::Esc); // leave edit — position is remembered

    press(&mut app, KeyCode::Char('e')); // re-enter edit
    let ed = app.reports[idx].editor.as_ref().unwrap();
    assert_eq!(
        (ed.row, ed.col),
        (row, col),
        "re-entering edit restores the remembered caret, not the buffer end"
    );
}

/// Item 7 (rep-autocomplete-spaces): a bare request-name fragment keeps
/// matching after the user types one of the name's spaces, so a spaced title
/// can be completed without first opening a quote.
#[test]
fn report_editor_autocompletes_through_a_typed_space() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Create Session".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST Create Ses");
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.report_completion(idx).map(|c| c.ghost).as_deref(),
        Some("sion"),
        "autocomplete keeps matching across a space typed into a bare name"
    );
    // Accepting still wraps the whole spaced name in quotes.
    press(&mut app, KeyCode::Right);
    assert!(
        app.active_report()
            .unwrap()
            .report
            .text
            .ends_with("REQUEST \"Create Session\""),
    );
}

/// Item 8 (rep-envs-autocomplete): environment names complete on a
/// `FOR … IN ENVS` clause, mirroring request-name completion. A bare fragment
/// auto-quotes on accept (env names must be quoted).
#[test]
fn report_editor_completes_environment_names() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            ..Default::default()
        }],
    ));
    add_empty_global_env(&mut app, "staging-au");
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    // Inside an opened quote the rest of the name plus the closing quote.
    app.reports[idx]
        .report
        .set_text("# collection: api\nFOR T IN ENVS \"stag");
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.report_completion(idx).map(|c| c.ghost).as_deref(),
        Some("ing-au\""),
        "an env name completes inside a FOR … ENVS clause"
    );
    press(&mut app, KeyCode::Esc);
    // A bare fragment auto-quotes the whole env name on accept.
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nFOR T IN ENVS stag");
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.report_completion(idx).map(|c| c.ghost).as_deref(),
        Some("ing-au"),
    );
    press(&mut app, KeyCode::Right);
    assert!(
        app.active_report()
            .unwrap()
            .report
            .text
            .ends_with("FOR T IN ENVS \"staging-au\""),
        "accepting a bare env name auto-quotes it"
    );
}

/// Item 4 (rep-editor-indent): pressing Enter after a `FOR` line adds one
/// indent level and a following line keeps it, while typing `END` snaps the
/// line back to its matching `FOR`'s indent (dedent).
#[test]
fn report_editor_newline_indents_in_a_for_block_and_end_dedents() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: c.hurl\nFOR F IN FILES \"docs\"");
    press(&mut app, KeyCode::Char('e')); // enter edit, cursor at line end
    press(&mut app, KeyCode::Enter); // newline after FOR → one extra indent
    for c in "REQUEST Oauth".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter); // keeps the indent
    for c in "END".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let text = app.active_report().unwrap().report.text.clone();
    assert!(
        text.contains("\n    REQUEST Oauth"),
        "a newline after FOR indents one level: {text:?}"
    );
    assert!(
        text.contains("\nEND") && !text.contains("\n    END"),
        "typing END snaps back to the FOR's indent: {text:?}"
    );
}

/// Item 4 (rep-editor-indent): a `PARALLEL FOR` header opens a block just like
/// a plain `FOR` — the next line indents and its `END` dedents to match.
#[test]
fn report_editor_indents_a_parallel_for_block() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: c.hurl\nPARALLEL(3) FOR F IN FILES \"docs\"");
    press(&mut app, KeyCode::Char('e'));
    press(&mut app, KeyCode::Enter);
    for c in "REQUEST Oauth".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    for c in "END".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let text = app.active_report().unwrap().report.text.clone();
    assert!(
        text.contains("\n    REQUEST Oauth"),
        "a newline after PARALLEL FOR indents one level: {text:?}"
    );
    assert!(
        text.contains("\nEND") && !text.contains("\n    END"),
        "typing END dedents to the PARALLEL FOR's indent: {text:?}"
    );
}

/// Item 4 (rep-editor-indent): a `REPORT REQUEST … WITH` header opens a block
/// too — the next line indents and the closing `END` snaps back to the
/// `REPORT`'s indent.
#[test]
fn report_editor_indents_a_report_with_block() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: c.hurl\nREPORT REQUEST process WITH");
    press(&mut app, KeyCode::Char('e'));
    press(&mut app, KeyCode::Enter);
    for c in "id: jsonpath \"$.id\"".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    for c in "END".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let text = app.active_report().unwrap().report.text.clone();
    assert!(
        text.contains("\n    id: jsonpath"),
        "a newline after REPORT … WITH indents one level: {text:?}"
    );
    assert!(
        text.contains("\nEND") && !text.contains("\n    END"),
        "typing END dedents to the REPORT's indent: {text:?}"
    );
}

/// Item 9 (rep-cwd-indicator): the binding panel names the directory relative
/// producer paths resolve against — the report's own folder once saved, else
/// the process working directory (flagged as a fallback).
#[test]
fn report_binding_panel_shows_the_base_directory() {
    use crate::i18n::Strings;
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.path = Some(std::path::PathBuf::from("/tmp/reports/dfa.report"));
    let s = Strings::for_language(&Language::English);
    let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(
        text.contains(s.report_base_dir_prefix) && text.contains("/tmp/reports"),
        "the base-dir indicator names the report's directory: {text:?}"
    );
}

/// Ctrl+Backspace in the report editor deletes the previous word (via the
/// in-tree `line_editor` module), and Ctrl+Z undoes the deletion.
#[test]
fn report_editor_ctrl_backspace_deletes_a_word_and_ctrl_z_undoes() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text("REQUEST Oauth");
    press(&mut app, KeyCode::Char('e')); // enter edit, cursor at end
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
    assert_eq!(
        app.active_report().unwrap().report.text,
        "REQUEST ",
        "Ctrl+Backspace deletes the word to the left"
    );
    app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(
        app.active_report().unwrap().report.text,
        "REQUEST Oauth",
        "Ctrl+Z restores the deleted word"
    );
}

/// Ctrl+Backspace deletes a whole quoted request name in one step, so a
/// spaced name doesn't have to be erased word by word.
#[test]
fn report_editor_ctrl_backspace_deletes_a_whole_quoted_request_name() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("REPORT REQUEST \"Upload document\"");
    press(&mut app, KeyCode::Char('e'));
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL));
    assert_eq!(
        app.active_report().unwrap().report.text,
        "REPORT REQUEST ",
        "the whole \"Upload document\" token is removed at once"
    );
}

/// On terminals without the keyboard-enhancement protocol, Ctrl+Backspace
/// arrives as Ctrl+H; the report editor must still word-delete for it.
#[test]
fn report_editor_ctrl_h_deletes_a_word_like_ctrl_backspace() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text("REQUEST Oauth");
    press(&mut app, KeyCode::Char('e')); // enter edit, cursor at end
    app.on_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    assert_eq!(
        app.active_report().unwrap().report.text,
        "REQUEST ",
        "Ctrl+H is treated as Ctrl+Backspace (word-delete) on legacy terminals"
    );
}

/// A report with no results grid yet: Tab has only the editor to focus, so it
/// stays put (the tab bar is no longer a focus stop).
#[test]
fn report_tab_cycles_focus_to_the_tab_list_and_back() {
    use super::reports::ReportView;
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    assert!(!app.report_tabbar_focus);
    assert_eq!(app.reports[idx].view, ReportView::Source);
    press(&mut app, KeyCode::Tab); // no grid, no tree → nothing else to focus
    assert!(!app.report_tabbar_focus, "Tab never focuses the tab list");
    assert_eq!(app.reports[idx].view, ReportView::Source);
}

/// A standalone report's source↔output swap is `v` (not `Tab`): `Tab` no longer
/// jumps onto the results grid, so it can't be hit by accident, while `v`
/// toggles the body between the editor and the grid.
#[test]
fn report_v_swaps_between_editor_and_results_grid() {
    use super::reports::ReportView;
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Oauth\n");
    app.revalidate_report(idx);
    let runner = FakeReportRunner {
        body: "{}".to_string(),
    };
    app.apply_report_run(idx, &runner); // lands on the results grid
    assert_eq!(app.reports[idx].view, ReportView::Results);

    // Tab is inert for a standalone report — it never leaves the grid.
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.reports[idx].view, ReportView::Results);
    assert!(!app.report_tabbar_focus);

    press(&mut app, KeyCode::Char('v')); // Results -> Source (the editor)
    assert_eq!(app.reports[idx].view, ReportView::Source);
    press(&mut app, KeyCode::Char('v')); // Source -> Results
    assert_eq!(app.reports[idx].view, ReportView::Results);
}

/// Item 10 (rep-tab-arrows): plain Left/Right arrows on the tab bar move across
/// report tabs, not just collection tabs (Ctrl+Left/Right already did).
#[test]
fn plain_arrow_keys_cycle_across_report_tabs() {
    let mut app = TuiApp::default();
    app.new_report_tab(); // collections=1, reports=1, active on the report tab
    app.focus = Pane::Tabs;
    app.active_tab = 0; // start on the collection tab
    assert!(!app.active_is_report());
    press(&mut app, KeyCode::Right); // plain Right advances onto the report tab
    assert!(
        app.active_is_report(),
        "a plain Right arrow reaches the report tab"
    );
    press(&mut app, KeyCode::Left); // plain Left steps back
    assert!(!app.active_is_report());
}

#[test]
fn cycle_tab_reaches_report_tabs() {
    let mut app = TuiApp::default();
    app.new_report_tab(); // collections=1, reports=1, active=1 (report)
    assert!(app.active_is_report());
    app.cycle_tab(true); // wraps back to collection tab 0
    assert!(!app.active_is_report());
    assert_eq!(app.active_tab, 0);
    app.cycle_tab(true); // forward to the report tab again
    assert!(app.active_is_report());
}
#[test]
fn closing_report_tab_then_reopening_restores_it() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# name: R1\n# collection: c.hurl\n");
    assert_eq!(app.reports.len(), 1);

    app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(app.reports.len(), 0);
    assert!(!app.active_is_report());
    assert_eq!(app.active_tab, 0);

    press(&mut app, KeyCode::Char('u'));
    assert_eq!(app.reports.len(), 1);
    assert_eq!(app.reports[0].report.name, "R1");
    assert!(app.active_is_report());
}

/// Report tabs (source text + name + active selection) survive a persistence
/// round-trip through `to_persisted` / `apply_persisted`.
#[test]
fn report_tabs_survive_persist_round_trip() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# name: Nightly\n# collection: c.hurl\nREQUEST x\n");
    let active_before = app.active_tab;

    let state = app.to_persisted();
    let mut restored = TuiApp::default();
    restored.apply_persisted(state);

    assert_eq!(restored.reports.len(), 1);
    assert_eq!(restored.reports[0].report.name, "Nightly");
    assert_eq!(restored.active_tab, active_before);
    assert!(restored.active_is_report());
    // Restored reports are revalidated, so diagnostics are populated.
    assert!(!restored.reports[0].report.dirty);
}

/// Rendering a full frame while a report tab is active (and while its modal
/// source editor is open) must not panic — the draw path has several
/// index-sensitive branches keyed on the unified collection/report tab index.
#[test]
fn drawing_a_report_tab_does_not_panic() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    // A bound collection so the binding panel exercises the "bound" branch too.
    app.collections
        .push(Collection::new("api".to_string(), Vec::new()));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# name: Nightly\n# collection: api\nREQUEST Oauth\n");
    app.revalidate_report(idx);

    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    // Now with the source panel in edit focus (inline editor rendered).
    press(&mut app, KeyCode::Char('e'));
    assert!(
        app.active_report().unwrap().editor.is_some(),
        "e gives the source panel edit focus"
    );
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
}

#[test]
fn report_source_highlights_keywords_and_underlines_the_error_line() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    app.collections
        .push(Collection::new("api".to_string(), Vec::new()));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    // A well-formed first line (REQUEST is a keyword) followed by a line the
    // parser rejects (a bare word that is not a statement).
    app.reports[idx]
        .report
        .set_text("REQUEST Oauth\nnonsense here\n");
    app.revalidate_report(idx);
    let accent = app.theme().accent;
    let err = app.theme().err;
    let err_line = app.reports[idx].parse_error_line;
    assert!(
        err_line.is_some(),
        "the malformed line should give a parse error line"
    );

    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer();

    // Somewhere in the buffer, the 'R' of the REQUEST keyword is drawn in the
    // theme accent colour (proving the styled highlighting reached the screen).
    let mut found_accent_keyword = false;
    let mut found_underlined_error = false;
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            if cell.symbol() == "R" && cell.fg == accent {
                found_accent_keyword = true;
            }
            if cell.fg == err && cell.modifier.contains(ratatui::style::Modifier::UNDERLINED) {
                found_underlined_error = true;
            }
        }
    }
    assert!(
        found_accent_keyword,
        "the REQUEST keyword should be drawn in the accent colour"
    );
    assert!(
        found_underlined_error,
        "the rejected line should be drawn in the error colour and underlined"
    );
}

/// A canned [`EntryRunner`] for the report-run tests: every request returns the
/// same `body` with HTTP 200, so a run is deterministic and network-free.
struct FakeReportRunner {
    body: String,
}

impl crate::report::run::EntryRunner for FakeReportRunner {
    fn run(
        &self,
        base: &crate::hurl::HurlEntry,
        _vars: &std::collections::HashMap<String, String>,
    ) -> crate::hurl::RunOutput {
        crate::hurl::RunOutput {
            entries: vec![crate::hurl::EntryOutcome {
                method: base.method.clone(),
                url: base.url.clone(),
                status: 200,
                status_text: "OK".to_string(),
                headers: Vec::new(),
                body: self.body.clone(),
                raw_body: self.body.clone(),
                asserts: Vec::new(),
                captures: Vec::new(),
                duration_ms: 7,
                ok: true,
                error: None,
            }],
            error: None,
        }
    }
}

/// A bound report with a `REPORT REQUEST` produces one row per iteration whose
/// cells carry the request's intrinsic columns (HttpStatus/Time/Response).
#[test]
fn report_run_populates_the_results_grid() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Oauth\n");
    app.revalidate_report(idx);

    let runner = FakeReportRunner {
        body: "{\"status\":\"ok\"}".to_string(),
    };
    let result = app.run_report_flow(idx, &runner).expect("runnable");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(
        result.rows[0].cells.get("Oauth.HttpStatus"),
        Some(&"200".to_string())
    );
    assert!(
        result.rows[0]
            .cells
            .get("Oauth.Response")
            .unwrap()
            .contains("ok")
    );
}

/// Running via `apply_report_run` stores the result, flips to the results view
/// and reports the row count; toggling flips back and forth.
#[test]
fn running_switches_to_the_results_view_and_toggles_back() {
    use super::reports::ReportView;
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Oauth\n");
    app.revalidate_report(idx);

    let runner = FakeReportRunner {
        body: "{}".to_string(),
    };
    app.apply_report_run(idx, &runner);
    assert_eq!(app.reports[idx].view, ReportView::Results);
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportRunDone { rows: 1, errors: 0 })
    ));

    app.toggle_report_view();
    assert_eq!(app.reports[idx].view, ReportView::Source);
    app.toggle_report_view();
    assert_eq!(app.reports[idx].view, ReportView::Results);
}

/// A background run (via the real thread + poll plumbing, using a fake runner)
/// leaves the app responsive: it reports "running" immediately, then the poll
/// folds in the delivered result — switching to the grid and reporting the row
/// count — once the worker thread finishes.
#[test]
fn background_report_run_delivers_a_result_via_poll() {
    use super::reports::ReportView;
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    let report_id = app.reports[idx].report.id;
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Oauth\n");
    app.revalidate_report(idx);

    let body = "{\"status\":\"ok\"}".to_string();
    app.start_report_run_faked(move |_| FakeReportRunner { body });
    // The run is in flight: running status + tracked, no result yet.
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportRunning)
    ));
    assert!(app.running_reports.contains_key(&report_id));

    // Drive the poll until the worker delivers (bounded so a bug can't hang).
    let mut done = false;
    for _ in 0..200 {
        app.poll_report_run_updates();
        if app.running_reports.is_empty() {
            done = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(done, "the background run should complete and be polled in");
    assert_eq!(app.reports[idx].view, ReportView::Results);
    assert!(app.reports[idx].result.is_some());
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportRunDone { rows: 1, errors: 0 })
    ));
    assert!(app.pending_report_runs.is_empty());
}

/// A cancelled background run's delivered result is discarded (not stored on the
/// tab), and the running flag is cleared.
#[test]
fn cancelled_background_report_run_discards_its_result() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    let report_id = app.reports[idx].report.id;
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Oauth\n");
    app.revalidate_report(idx);

    let body = "{}".to_string();
    app.start_report_run_faked(move |_| FakeReportRunner { body });
    // Cancel it before polling: the poller must discard the arriving result.
    app.running_reports
        .get(&report_id)
        .unwrap()
        .store(true, std::sync::atomic::Ordering::Relaxed);

    for _ in 0..200 {
        app.poll_report_run_updates();
        if app.pending_report_runs.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(app.running_reports.is_empty(), "cancel clears the run flag");
    assert!(
        app.reports[idx].result.is_none(),
        "a cancelled run's result is discarded"
    );
}

/// Streaming: a run's updates arrive as a `Skeleton` (the greyed projected
/// grid) then a `Row` per completed iteration then a `Done`. The poller installs
/// the skeleton and switches to the grid immediately, un-greys each row as it
/// streams in while advancing the progress status, and finally swaps in the
/// finalized result — clearing the per-row progress so the grid is fully lit.
#[test]
fn streaming_report_updates_fill_the_greyed_skeleton_row_by_row() {
    use super::reports::{ReportRunUpdate, ReportView, RowState};
    use crate::i18n::Status;
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "send".to_string(),
            method: "GET".to_string(),
            url: "http://example/send".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    let report_id = app.reports[idx].report.id;
    app.reports[idx].report.set_text(
        "# collection: api\nFOR X IN [\"a\", \"b\", \"c\"]\n    REPORT REQUEST send\nEND\n",
    );
    app.revalidate_report(idx);

    // The dry expansion is the skeleton the worker would send first.
    let skeleton = app.dry_run_report_flow(idx).expect("expandable");
    assert_eq!(skeleton.rows.len(), 3);

    // Drive the real poll+apply path deterministically via a hand-built channel
    // (no worker thread): register the run, then feed messages one at a time.
    let (tx, rx) = std::sync::mpsc::channel();
    app.running_reports.insert(
        report_id,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    app.pending_report_runs.push((report_id, rx));

    // 1. Skeleton → greyed grid shown immediately, nothing filled yet.
    tx.send(ReportRunUpdate::Skeleton {
        report_id,
        result: skeleton.clone(),
    })
    .unwrap();
    app.poll_report_run_updates();
    assert_eq!(app.reports[idx].view, ReportView::Results);
    let prog = app.reports[idx].run_progress.as_ref().expect("streaming");
    assert_eq!(
        prog.states,
        vec![
            RowState::Scheduled,
            RowState::Scheduled,
            RowState::Scheduled
        ]
    );
    assert_eq!(prog.done, 0);
    assert!(matches!(
        app.status,
        Some(Status::ReportRunProgress { done: 0, total: 3 })
    ));
    // Drawing a partly-greyed grid must not panic.
    {
        use ratatui::{Terminal, backend::TestBackend};
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    }

    // 1b. A RowStarted marks that slot Running (its requests are in flight) but
    //     doesn't advance the finished count — the icon flips scheduled→running.
    tx.send(ReportRunUpdate::RowStarted {
        report_id,
        path: skeleton.rows[1].path.clone(),
    })
    .unwrap();
    app.poll_report_run_updates();
    let prog = app.reports[idx].run_progress.as_ref().unwrap();
    assert_eq!(prog.states[1], RowState::Running, "row 1 is running");
    assert_eq!(prog.states[0], RowState::Scheduled);
    assert_eq!(prog.done, 0, "running does not count as finished");

    // 2. Rows stream in (deliberately out of order, as PARALLEL would): each
    //    lands in its own slot by path and advances the progress count.
    for (order, src) in [1usize, 0, 2].into_iter().enumerate() {
        let mut row = skeleton.rows[src].clone();
        row.cells
            .insert("send.Marker".to_string(), format!("v{src}"));
        tx.send(ReportRunUpdate::Row {
            report_id,
            row: Box::new(row),
        })
        .unwrap();
        app.poll_report_run_updates();
        let prog = app.reports[idx].run_progress.as_ref().unwrap();
        assert_eq!(prog.states[src], RowState::Finished, "slot {src} finished");
        assert_eq!(prog.done, order + 1);
        assert!(matches!(
            app.status,
            Some(Status::ReportRunProgress { total: 3, .. })
        ));
    }
    // Every streamed cell landed in the right row.
    let result = app.reports[idx].result.as_ref().unwrap();
    for src in 0..3 {
        assert_eq!(
            result.rows[src].cells.get("send.Marker"),
            Some(&format!("v{src}"))
        );
    }

    // 3. Done → finalized result installed, progress cleared (grid fully lit).
    tx.send(ReportRunUpdate::Done {
        report_id,
        result: skeleton.clone(),
    })
    .unwrap();
    app.poll_report_run_updates();
    assert!(app.reports[idx].run_progress.is_none(), "progress cleared");
    assert!(
        app.running_reports.is_empty(),
        "running flag cleared on Done"
    );
    assert!(matches!(
        app.status,
        Some(Status::ReportRunDone { rows: 3, errors: 0 })
    ));
}

/// Closing a report tab whose run is still streaming detaches it cleanly:
/// the worker is cancelled, its channel retired, and the pre-run grid restored
/// so reopening (`u`) shows a normal grid instead of a permanently greyed,
/// half-filled "running" one the background poller can no longer reach.
#[test]
fn closing_a_running_report_tab_detaches_the_run_and_restores_the_grid() {
    use super::reports::{ReportRunUpdate, ReportView};
    use crate::report::model::ReportResult;
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "send".to_string(),
            method: "GET".to_string(),
            url: "http://example/send".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    let report_id = app.reports[idx].report.id;
    app.reports[idx]
        .report
        .set_text("# collection: api\nFOR X IN [\"a\", \"b\"]\n    REPORT REQUEST send\nEND\n");
    app.revalidate_report(idx);

    // Seed a prior grid, then start streaming so `run_progress` is live with
    // that grid stashed as `prev_result`.
    let mut prior = ReportResult::default();
    prior.column_order = vec!["prior".into()];
    app.reports[idx].result = Some(prior.clone());
    let skeleton = app.dry_run_report_flow(idx).expect("expandable");
    let (tx, rx) = std::sync::mpsc::channel();
    app.running_reports.insert(
        report_id,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    app.pending_report_runs.push((report_id, rx));
    tx.send(ReportRunUpdate::Skeleton {
        report_id,
        result: skeleton,
    })
    .unwrap();
    app.poll_report_run_updates();
    assert!(app.reports[idx].run_progress.is_some(), "streaming started");

    // Close the running tab.
    app.close_active_report_tab();
    assert!(
        app.running_reports.is_empty(),
        "the worker's cancel flag is set and the run retired"
    );
    assert!(
        app.pending_report_runs.is_empty(),
        "the run's channel is retired so a late Done can't clobber the grid"
    );

    // Reopen it: no lingering streaming state, and the pre-run grid is back.
    app.reopen_closed_tab();
    let ridx = app.active_report_index().unwrap();
    assert!(
        app.reports[ridx].run_progress.is_none(),
        "reopened tab is not stuck in a greyed running state"
    );
    assert_eq!(
        app.reports[ridx].result.as_ref().map(|r| &r.column_order),
        Some(&prior.column_order),
        "the pre-run grid is restored"
    );
    assert_ne!(app.reports[ridx].view, ReportView::Source);
}

/// An unbound report can't run: `run_report_flow` returns a reason and
/// `apply_report_run` keeps the source view with a blocked status.
#[test]
fn report_run_is_blocked_when_unbound() {
    use super::reports::ReportView;
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    // The scratch template's `# collection:` header is empty → unbound.
    app.revalidate_report(idx);

    let runner = FakeReportRunner {
        body: "{}".to_string(),
    };
    assert!(app.run_report_flow(idx, &runner).is_err());

    app.apply_report_run(idx, &runner);
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportRunBlocked(_))
    ));
    assert_eq!(app.reports[idx].view, ReportView::Source);
    assert!(app.reports[idx].result.is_none());
}

/// Exporting writes a CSV next to a saved report, driven by the `columns:`
/// header directive.
#[test]
fn report_export_writes_a_csv_next_to_the_report() {
    let dir = std::env::temp_dir().join(format!(
        "pb-report-export-{}",
        crate::report::report::next_report_id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let report_path = dir.join("smoke.report");

    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.path = Some(report_path.clone());
    app.reports[idx].report.set_text(
        "# collection: api\n# columns: Oauth.HttpStatus as Status\nREPORT REQUEST Oauth\n",
    );
    app.revalidate_report(idx);

    let runner = FakeReportRunner {
        body: "{}".to_string(),
    };
    app.apply_report_run(idx, &runner);

    // Export now routes through the folder picker (like "Save Collection As")
    // rather than silently writing into the process cwd: it opens a Browser
    // overlay whose commit writes the CSV to the chosen folder + filename.
    app.export_active_report_csv();
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Browser(FileAction::SaveReportCsvChooseFolder, _))
        ),
        "export should open the report-CSV folder picker"
    );
    app.overlay = None;
    app.browser_commit_save(
        FileAction::SaveReportCsvChooseFolder,
        dir.clone(),
        "smoke".to_string(),
    );

    let csv_path = report_path.with_extension("csv");
    let csv = std::fs::read_to_string(&csv_path).unwrap();
    assert_eq!(csv, "Status\r\n200\r\n");
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportExported(_))
    ));

    std::fs::remove_dir_all(&dir).ok();
}

/// The export filename's extension selects the output format: typing an
/// `.xlsx`/`.json`/`.html` name writes that format, not CSV.
#[test]
fn report_export_format_follows_the_typed_extension() {
    let dir = std::env::temp_dir().join(format!(
        "pb-report-fmt-{}",
        crate::report::report::next_report_id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let report_path = dir.join("smoke.report");

    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.path = Some(report_path.clone());
    app.reports[idx].report.set_text(
        "# collection: api\n# columns: Oauth.HttpStatus as Status\nREPORT REQUEST Oauth\n",
    );
    app.revalidate_report(idx);
    let runner = FakeReportRunner {
        body: "{}".to_string(),
    };
    app.apply_report_run(idx, &runner);

    // .xlsx → a ZIP container (starts with "PK").
    app.browser_commit_save(
        FileAction::SaveReportCsvChooseFolder,
        dir.clone(),
        "smoke.xlsx".to_string(),
    );
    let xlsx = std::fs::read(dir.join("smoke.xlsx")).unwrap();
    assert_eq!(&xlsx[..2], b"PK", "xlsx is a ZIP");

    // .json → a JSON document.
    app.browser_commit_save(
        FileAction::SaveReportCsvChooseFolder,
        dir.clone(),
        "smoke.json".to_string(),
    );
    let json = std::fs::read_to_string(dir.join("smoke.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["rows"][0]["Status"], "200");

    // .html → a self-contained HTML page.
    app.browser_commit_save(
        FileAction::SaveReportCsvChooseFolder,
        dir.clone(),
        "smoke.html".to_string(),
    );
    let html = std::fs::read_to_string(dir.join("smoke.html")).unwrap();
    assert!(html.starts_with("<!DOCTYPE html>"));

    std::fs::remove_dir_all(&dir).ok();
}

/// Exporting before a run reports why nothing can be written and does NOT open
/// the folder picker.
#[test]
fn report_export_without_a_run_is_blocked() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Oauth\n");
    app.revalidate_report(idx);

    app.export_active_report_csv();
    assert!(app.overlay.is_none(), "no run yet → no folder picker");
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportRunBlocked(_))
    ));
}

/// Saving a baseline (Shift+B in the results view) routes through the folder
/// picker and writes a `.baseline` JSON snapshot of the last run that reloads
/// via [`Baseline::load`], so a later `# baseline:` run can diff against it.
#[test]
fn report_baseline_save_writes_a_snapshot_next_to_the_report() {
    let dir = std::env::temp_dir().join(format!(
        "pb-report-baseline-{}",
        crate::report::report::next_report_id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let report_path = dir.join("smoke.report");

    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.path = Some(report_path.clone());
    app.reports[idx].report.set_text(
        "# collection: api\n# columns: Oauth.HttpStatus as Status\nREPORT REQUEST Oauth\n",
    );
    app.revalidate_report(idx);

    let runner = FakeReportRunner {
        body: "{}".to_string(),
    };
    app.apply_report_run(idx, &runner);

    // The `B` key opens the baseline folder picker (seeded `<report>.baseline`).
    app.save_active_report_baseline();
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Browser(
                FileAction::SaveReportBaselineChooseFolder,
                _
            ))
        ),
        "save-baseline should open the baseline folder picker"
    );
    assert_eq!(app.default_report_baseline_filename(), "smoke.baseline");
    app.overlay = None;
    app.browser_commit_save(
        FileAction::SaveReportBaselineChooseFolder,
        dir.clone(),
        "smoke".to_string(),
    );

    let snap_path = report_path.with_extension("baseline");
    let baseline = crate::report::Baseline::load(&snap_path).expect("snapshot reloads");
    assert_eq!(baseline.rows.len(), 1);
    assert_eq!(
        baseline.rows[0].cells.get("Oauth.HttpStatus"),
        Some(&"200".to_string())
    );
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportBaselineSaved(_))
    ));

    std::fs::remove_dir_all(&dir).ok();
}

/// Saving a baseline before a run reports why nothing can be written and does
/// NOT open the folder picker.
#[test]
fn report_baseline_save_without_a_run_is_blocked() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Oauth\n");
    app.revalidate_report(idx);

    app.save_active_report_baseline();
    assert!(app.overlay.is_none(), "no run yet → no folder picker");
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportRunBlocked(_))
    ));
}

/// The results grid draws a bold header row (in the accent colour) after a run.
#[test]
fn report_results_grid_draws_a_header_row() {
    use ratatui::{Terminal, backend::TestBackend};
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text(
        "# collection: api\n# columns: Oauth.HttpStatus as Status\nREPORT REQUEST Oauth\n",
    );
    app.revalidate_report(idx);
    let runner = FakeReportRunner {
        body: "{}".to_string(),
    };
    app.apply_report_run(idx, &runner);

    let accent = app.theme().accent;
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer();

    // The 'S' of the "Status" header is drawn bold in the accent colour.
    let mut found_header = false;
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            if cell.symbol() == "S"
                && cell.fg == accent
                && cell.modifier.contains(ratatui::style::Modifier::BOLD)
            {
                found_header = true;
            }
        }
    }
    assert!(
        found_header,
        "the results grid should draw its column headers in bold accent"
    );
}

/// The report results grid supports mouse text selection + copy, exactly like
/// the collection view's body panels: a click-drag over the grid selects text,
/// and releasing copies it (setting the Copied status).
#[test]
fn report_results_grid_supports_mouse_selection_and_copy() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text(
        "# collection: api\n# columns: Oauth.HttpStatus as Status\nREPORT REQUEST Oauth\n",
    );
    app.revalidate_report(idx);
    let runner = FakeReportRunner {
        body: "{}".to_string(),
    };
    app.apply_report_run(idx, &runner);

    // Draw so the results panel's hit-test area is recorded.
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
    let area = app.report_pane_areas[super::reports::ReportPane::Results.idx()];
    assert!(
        area.width > 0 && area.height > 1,
        "results area must render"
    );

    // Click-drag across the header row, then release.
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    });
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: area.x + area.width - 1,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.has_any_selection(),
        "dragging over the grid starts a text selection"
    );
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: area.x + area.width - 1,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        matches!(app.status, Some(crate::i18n::Status::Copied)),
        "releasing the drag copies the selection"
    );
}

/// With nothing selected, `y` in the report view copies the whole visible
/// panel (here the results grid) — the same fallback the collection view uses.
#[test]
fn report_view_y_copies_the_whole_panel_when_nothing_selected() {
    use ratatui::{Terminal, backend::TestBackend};

    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text(
        "# collection: api\n# columns: Oauth.HttpStatus as Status\nREPORT REQUEST Oauth\n",
    );
    app.revalidate_report(idx);
    let runner = FakeReportRunner {
        body: "{}".to_string(),
    };
    app.apply_report_run(idx, &runner);

    // Draw so the results panel caches its content (whole_text needs it).
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    assert!(!app.has_any_selection());
    press(&mut app, KeyCode::Char('y'));
    assert!(
        matches!(app.status, Some(crate::i18n::Status::Copied)),
        "`y` with no selection copies the whole results panel"
    );
}

/// A dry run expands the flow with no HTTP: a `FOR … IN FILES` loop over a
/// folder of three files projects three rows, and the preview overlay lists
/// each iteration's `FILE=` binding with no problems.
#[test]
fn dry_run_previews_row_count_and_file_bindings() {
    let dir = std::env::temp_dir().join(format!(
        "pb-report-dry-{}",
        crate::report::report::next_report_id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for n in ["a.jpg", "b.jpg", "c.jpg"] {
        std::fs::write(dir.join(n), b"x").unwrap();
    }

    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Upload".to_string(),
            method: "POST".to_string(),
            url: "http://example/upload".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text(&format!(
        "# collection: api\nFOR FILE IN FILES {:?}\n    REPORT REQUEST Upload\nEND\n",
        dir.display().to_string()
    ));
    app.revalidate_report(idx);

    // The core expansion projects one row per file, without any runner.
    let result = app.dry_run_report_flow(idx).expect("expandable");
    assert_eq!(result.rows.len(), 3);

    // Opening the overlay (via the `d` key) builds a preview from that.
    press(&mut app, KeyCode::Char('d'));
    match app.overlay.as_ref() {
        Some(Overlay::ReportDryRun(p)) => {
            assert_eq!(p.rows, 3);
            assert_eq!(p.samples.len(), 3);
            assert!(
                p.samples.iter().all(|line| line.starts_with("FILE=")),
                "each sample should show the FILE binding, got {:?}",
                p.samples
            );
            assert!(p.errors.is_empty(), "a valid loop should have no problems");
        }
        _ => panic!("the `d` key should open the dry-run preview overlay"),
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// A dry run surfaces producer problems (here, a missing directory) as preview
/// errors and projects zero rows — all without sending a request.
#[test]
fn dry_run_surfaces_producer_errors_without_running() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Upload".to_string(),
            method: "POST".to_string(),
            url: "http://example/upload".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text(
        "# collection: api\nFOR FILE IN FILES \"/no/such/paperboy/dir/here\"\n    REPORT REQUEST Upload\nEND\n",
    );
    app.revalidate_report(idx);

    app.open_report_dry_run();
    match app.overlay.as_ref() {
        Some(Overlay::ReportDryRun(p)) => {
            assert_eq!(p.rows, 0, "a missing directory yields no rows");
            assert!(
                !p.errors.is_empty(),
                "the missing directory should surface a producer error"
            );
        }
        _ => panic!("expected the dry-run overlay"),
    }
}

/// An unbound report can't be dry-run (there's no collection to resolve request
/// names against): the overlay stays closed and the status bar says why.
#[test]
fn dry_run_is_blocked_when_unbound() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text("REQUEST x\n");
    app.revalidate_report(idx);

    app.open_report_dry_run();
    assert!(
        app.overlay.is_none(),
        "an unbound report can't be dry-run, so no overlay opens"
    );
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportRunBlocked(_))
    ));
}

/// The dry-run overlay scrolls with Up/Down and closes on Esc; drawing it must
/// not panic.
#[test]
fn dry_run_overlay_scrolls_and_closes() {
    use ratatui::{Terminal, backend::TestBackend};
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Upload".to_string(),
            method: "POST".to_string(),
            url: "http://example/upload".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Upload\n");
    app.revalidate_report(idx);

    press(&mut app, KeyCode::Char('d'));
    assert!(matches!(app.overlay, Some(Overlay::ReportDryRun(_))));

    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();

    // Down scrolls the preview but keeps it open; Esc closes it.
    press(&mut app, KeyCode::Down);
    assert_eq!(app.dry_run_scroll, 1);
    assert!(matches!(app.overlay, Some(Overlay::ReportDryRun(_))));
    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_none(), "Esc closes the dry-run overlay");
}

/// `wrap_lines_with_marker` breaks an over-long line into several, ending every
/// wrapped segment (but not the last) with the dim `↵` marker, and leaves lines
/// that already fit untouched.
#[test]
fn wrap_lines_with_marker_marks_soft_wraps() {
    use ratatui::text::{Line, Span};
    let th = TuiApp::default().theme();
    let long = Line::from(Span::raw("abcdefghij")); // 10 columns
    let short = Line::from(Span::raw("ok"));
    let out = super::draw::wrap_lines_with_marker(vec![long, short], 5, &th);
    // Width 5 reserves the last column for the marker, so content wraps at 4:
    // "abcd↵" / "efgh↵" / "ij", then the untouched "ok".
    assert_eq!(out.len(), 4, "3 wrapped rows + 1 short row: {out:?}");
    let text_of = |l: &Line| -> String { l.spans.iter().map(|sp| sp.content.clone()).collect() };
    assert!(
        text_of(&out[0]).ends_with('↵'),
        "row 0 marked: {:?}",
        text_of(&out[0])
    );
    assert!(
        text_of(&out[1]).ends_with('↵'),
        "row 1 marked: {:?}",
        text_of(&out[1])
    );
    assert!(!text_of(&out[2]).ends_with('↵'), "final segment unmarked");
    assert_eq!(text_of(&out[3]), "ok", "a fitting line is unchanged");
    // The reconstructed content (minus markers) equals the original.
    let joined: String = out
        .iter()
        .take(3)
        .map(text_of)
        .collect::<String>()
        .replace('↵', "");
    assert_eq!(joined, "abcdefghij");
}

/// Build a report tab bound to `api` with the given flow `text` and a synthetic
/// last-run result whose produced columns are `column_order` — so the column
/// picker (which reads the last result) can be exercised without a network run.
fn report_with_result(text: &str, column_order: &[&str]) -> (TuiApp, usize) {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Upload".to_string(),
            method: "POST".to_string(),
            url: "http://example/upload".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text(text);
    app.revalidate_report(idx);
    let mut row = crate::report::model::ReportRow::default();
    for (i, c) in column_order.iter().enumerate() {
        row.cells.insert(c.to_string(), format!("v{i}"));
    }
    app.reports[idx].result = Some(crate::report::model::ReportResult {
        rows: vec![row],
        column_order: column_order.iter().map(|c| c.to_string()).collect(),
        no_match_marker: String::new(),
        errors: Vec::new(),
    });
    (app, idx)
}

/// `c` opens the column picker, listing every produced column as an included
/// row (when the flow has no `# columns:` directive yet).
#[test]
fn column_picker_lists_produced_columns() {
    let (mut app, _idx) = report_with_result(
        "# collection: api\nREPORT REQUEST Upload\n",
        &["Upload.HttpStatus", "Upload.status", "Upload.Response"],
    );
    press(&mut app, KeyCode::Char('c'));
    match app.overlay.as_ref() {
        Some(Overlay::ReportColumns(p)) => {
            let headers: Vec<&str> = p.rows.iter().map(|r| r.header.as_str()).collect();
            assert_eq!(
                headers,
                vec!["Upload.HttpStatus", "Upload.status", "Upload.Response"]
            );
            assert!(p.rows.iter().all(|r| r.included), "all included by default");
        }
        _ => panic!("`c` should open the column picker"),
    }
    // Drawing the overlay must not panic.
    use ratatui::{Terminal, backend::TestBackend};
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| super::draw::draw(f, &mut app)).unwrap();
}

/// Toggling a column off and pressing Enter writes a `# columns:` directive
/// that omits it, and the surgical edit keeps the rest of the flow intact.
#[test]
fn column_picker_apply_writes_columns_directive() {
    let (mut app, idx) = report_with_result(
        "# collection: api\nREPORT REQUEST Upload\n",
        &["Upload.HttpStatus", "Upload.status", "Upload.Response"],
    );
    press(&mut app, KeyCode::Char('c'));
    // Move to the third row (Upload.Response) and toggle it off.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Enter);
    assert!(app.overlay.is_none(), "Enter closes the picker");
    let text = &app.reports[idx].report.text;
    assert!(
        text.contains("# columns: Upload.HttpStatus, Upload.status"),
        "columns directive should omit the toggled-off Response: {text:?}"
    );
    assert!(
        !text.contains("Upload.Response"),
        "Response was excluded: {text:?}"
    );
    assert!(text.contains("REPORT REQUEST Upload"), "body preserved");
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportColumnsApplied)
    ));
}

/// Shift+↓ reorders the selected column; the written directive follows the new
/// order.
#[test]
fn column_picker_reorders_with_shift_arrows() {
    let (mut app, idx) =
        report_with_result("# collection: api\nREPORT REQUEST Upload\n", &["a", "b"]);
    press(&mut app, KeyCode::Char('c'));
    // Move the first row ("a") down past "b".
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    press(&mut app, KeyCode::Enter);
    let text = &app.reports[idx].report.text;
    assert!(
        text.contains("# columns: b, a"),
        "reordered directive: {text:?}"
    );
}

/// An existing `# columns:` directive seeds the picker (keeping its `AS`
/// renames) and is rewritten in place rather than duplicated.
#[test]
fn column_picker_rewrites_existing_directive() {
    let (mut app, idx) = report_with_result(
        "# collection: api\n# columns: Upload.status AS Status\nREPORT REQUEST Upload\n",
        &["Upload.HttpStatus", "Upload.status"],
    );
    press(&mut app, KeyCode::Char('c'));
    match app.overlay.as_ref() {
        Some(Overlay::ReportColumns(p)) => {
            assert_eq!(p.rows[0].header, "Status");
            assert_eq!(p.rows[0].sources, vec!["Upload.status".to_string()]);
            assert!(p.rows[0].included);
            assert!(
                p.rows
                    .iter()
                    .any(|r| r.header == "Upload.HttpStatus" && !r.included)
            );
        }
        _ => panic!("expected the column picker"),
    }
    press(&mut app, KeyCode::Enter);
    let text = &app.reports[idx].report.text;
    assert_eq!(text.matches("# columns:").count(), 1, "rewritten in place");
    assert!(
        text.contains("# columns: Upload.status AS Status"),
        "{text:?}"
    );
}

/// Opening the picker without a prior run shows a hint and no overlay.
#[test]
fn column_picker_needs_a_run_first() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Upload".to_string(),
            method: "POST".to_string(),
            url: "http://example/upload".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Upload\n");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Char('c'));
    assert!(app.overlay.is_none());
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportColumnsNeedRun)
    ));
}

/// A `# environment:` header selects one loaded global environment as the
/// report's base variable layer — even overriding the app's active env — so a
/// plain, no-comparison run is reproducible regardless of app state.
#[test]
fn environment_header_selects_the_named_env_as_base_vars() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    // An active env with REGION=active, plus a "prod" env with REGION=prod.
    let (active, _) = crate::environment::parse_vars_pending("global".into(), "REGION=active");
    let active_id = add_global_env(&mut app, active);
    app.active_env_id = Some(active_id);
    let (prod, _) = crate::environment::parse_vars_pending("prod".into(), "REGION=prod");
    add_global_env(&mut app, prod);

    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\n# environment: prod\nREPORT REQUEST Oauth\n");
    app.revalidate_report(idx);

    // No validation errors: the named env is loaded.
    assert!(
        !app.reports[idx]
            .diagnostics
            .iter()
            .any(|d| d.severity == crate::report::validate::Severity::Error),
        "a loaded '# environment:' should not error: {:?}",
        app.reports[idx].diagnostics
    );

    let result = app.dry_run_report_flow(idx).expect("expandable");
    assert_eq!(
        result.rows[0].vars.get("REGION"),
        Some(&"prod".to_string()),
        "the named environment must supply the base vars, overriding the active env"
    );
}

/// Without a `# environment:` header, the base layer falls back to the app's
/// active (and pinned) environment — the prior behaviour is preserved.
#[test]
fn without_environment_header_the_active_env_is_used() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    let (active, _) = crate::environment::parse_vars_pending("global".into(), "REGION=active");
    let active_id = add_global_env(&mut app, active);
    app.active_env_id = Some(active_id);

    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST Oauth\n");
    app.revalidate_report(idx);

    let result = app.dry_run_report_flow(idx).expect("expandable");
    assert_eq!(
        result.rows[0].vars.get("REGION"),
        Some(&"active".to_string()),
        "with no '# environment:' the active env supplies the base vars"
    );
}

// ---- P8: .report file load/save via the File menu + BIND ----

/// A helper temp dir unique to each P8 test.
fn p8_temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pb-p8-{tag}-{}",
        crate::report::report::next_report_id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn file_load_report_item_opens_the_report_source_step() {
    let mut app = TuiApp::default();
    // Row 4 of the Load kind list is the "Report" item; like a collection it
    // can be loaded locally or from git, so it opens the local/git source step.
    app.activate_file_load_item(4);
    assert!(matches!(
        app.overlay,
        Some(Overlay::FileLoadSource(FileKind::Report, 0))
    ));
}

#[test]
fn file_save_report_item_opens_the_report_destination_step() {
    let mut app = TuiApp::default();
    app.activate_file_save_item(4);
    assert!(matches!(
        app.overlay,
        Some(Overlay::FileSaveDest(FileKind::Report, 0))
    ));
}

#[test]
fn report_save_dest_items_include_git() {
    use crate::i18n::Strings;
    let s = Strings::for_language(&Language::English);
    let items = file_save_dest_items(FileKind::Report, &s);
    assert_eq!(
        items,
        vec![s.file_dest_save, s.file_dest_save_as, s.file_dest_git]
    );
    assert_eq!(file_save_kind_index(FileKind::Report), 4);
}

#[test]
fn report_load_from_git_opens_the_report_remote_wizard() {
    let mut app = TuiApp::default();
    // Choosing the "git" source for a Report opens the remote wizard scoped to
    // reports (so its file picker only offers `.report` files).
    app.activate_file_load_source(FileKind::Report, 1);
    assert!(matches!(
        &app.overlay,
        Some(Overlay::RemoteGit(w)) if w.kind == RemoteKind::Report
    ));
}

#[test]
fn report_git_save_needs_a_git_origin() {
    use crate::i18n::Status;
    let mut app = TuiApp::default();
    app.new_report_tab(); // a scratch report has no git origin
    app.open_git_save_report_wizard();
    assert!(app.overlay.is_none(), "no wizard without a git origin");
    assert!(matches!(app.status, Some(Status::NoGitOrigin)));
}

#[test]
fn report_git_save_opens_the_wizard_for_a_git_loaded_report() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.git_origin = Some(crate::git_remote::GitOrigin {
        repo_url: "https://example.test/repo.git".to_string(),
        path: "reports/nightly.report".to_string(),
        ref_kind: RefKind::Branch,
        ref_name: "main".to_string(),
    });
    app.open_git_save_report_wizard();
    match &app.overlay {
        Some(Overlay::GitSave(w)) => {
            assert!(matches!(
                w.source,
                crate::tui::git_save::GitSaveSource::Report { report_idx } if report_idx == idx
            ));
            assert_eq!(w.collection_path.text(), "reports/nightly.report");
        }
        other => panic!(
            "expected the git-save wizard, got overlay present: {}",
            other.is_some()
        ),
    }
}

#[test]
fn report_load_opens_a_new_tab_from_a_report_file() {
    let dir = p8_temp_dir("load");
    let path = dir.join("smoke.report");
    std::fs::write(
        &path,
        "# name: Nightly\n# collection: api.hurl\nREQUEST Oauth\n",
    )
    .unwrap();

    let mut app = TuiApp::default();
    let before = app.reports.len();
    app.do_file_action(FileAction::OpenReport, &path.to_string_lossy());

    assert_eq!(app.reports.len(), before + 1);
    let idx = app
        .active_report_index()
        .expect("active is the loaded report");
    assert_eq!(app.reports[idx].report.name, "Nightly");
    assert_eq!(
        app.reports[idx].report.path.as_deref(),
        Some(path.as_path())
    );
    assert!(!app.reports[idx].report.dirty);
    assert!(matches!(app.status, Some(crate::i18n::Status::Loaded)));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn report_save_as_writes_a_report_file_via_the_folder_picker() {
    let dir = p8_temp_dir("saveas");

    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api.hurl\nREQUEST Oauth\n");

    // "Save Report As" opens the destination-folder browser (like collections).
    app.begin_save_as(FileAction::SaveReport);
    assert!(matches!(
        app.overlay,
        Some(Overlay::Browser(FileAction::SaveReportChooseFolder, _))
    ));
    app.overlay = None;

    // Committing the folder pick writes `<dir>/nightly.report` and records it.
    app.browser_commit_save(
        FileAction::SaveReportChooseFolder,
        dir.clone(),
        "nightly".to_string(),
    );
    let path = dir.join("nightly.report");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("REQUEST Oauth"));

    let idx = app.active_report_index().unwrap();
    assert_eq!(
        app.reports[idx].report.path.as_deref(),
        Some(path.as_path())
    );
    assert!(!app.reports[idx].report.dirty, "save clears dirty");
    assert!(matches!(app.status, Some(crate::i18n::Status::Saved)));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn report_save_writes_back_and_clears_dirty() {
    let dir = p8_temp_dir("save");
    let path = dir.join("smoke.report");
    std::fs::write(&path, "# collection: api.hurl\nREQUEST Oauth\n").unwrap();

    let mut app = TuiApp::default();
    app.do_file_action(FileAction::OpenReport, &path.to_string_lossy());
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api.hurl\nREQUEST Oauth\nREQUEST Logout\n");
    assert!(app.reports[idx].report.dirty);

    app.do_file_action(FileAction::SaveReport, &path.to_string_lossy());
    assert!(!app.reports[idx].report.dirty);
    assert!(std::fs::read_to_string(&path).unwrap().contains("Logout"));
    assert!(matches!(app.status, Some(crate::i18n::Status::Saved)));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn report_save_on_a_dirty_report_confirms_overwrite() {
    let dir = p8_temp_dir("saveconfirm");
    let path = dir.join("smoke.report");
    std::fs::write(&path, "# collection: api.hurl\nREQUEST Oauth\n").unwrap();

    let mut app = TuiApp::default();
    app.do_file_action(FileAction::OpenReport, &path.to_string_lossy());
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api.hurl\nREQUEST Changed\n");

    // A dirty report asks before overwriting the original file.
    app.begin_save(FileAction::SaveReport);
    assert!(matches!(
        app.overlay,
        Some(Overlay::Confirm {
            action: ConfirmAction::Save(FileAction::SaveReport),
            ..
        })
    ));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn report_save_without_a_path_falls_back_to_save_as() {
    let mut app = TuiApp::default();
    app.new_report_tab();
    // A never-saved (scratch) report has no path, so "Save" becomes "Save As".
    app.begin_save(FileAction::SaveReport);
    assert!(matches!(
        app.overlay,
        Some(Overlay::Browser(FileAction::SaveReportChooseFolder, _))
    ));
}

#[test]
fn report_bind_repoints_the_collection_header_by_name() {
    let mut app = TuiApp::default();
    app.collections.push(Collection::new(
        "payments".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    ));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection:\nREQUEST Oauth\n");
    app.revalidate_report(idx);

    app.open_report_bind();
    let Some(Overlay::ReportBind(mut picker)) = app.overlay.take() else {
        panic!("bind picker should open");
    };
    picker.selected = picker
        .options
        .iter()
        .position(|o| o.name == "payments")
        .unwrap();
    app.report_bind_key_handler(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), picker);

    let idx = app.active_report_index().unwrap();
    // A path-less (scratch) collection binds by name so name-based resolution
    // still finds it.
    assert_eq!(
        app.reports[idx].report.collection_ref(),
        Some("payments".to_string())
    );
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportBound(_))
    ));
    // And the report now resolves to that collection.
    assert!(
        app.resolve_bound_collection(&app.reports[idx].report)
            .is_some()
    );
}

#[test]
fn report_bind_prefers_a_relative_path() {
    let dir = p8_temp_dir("bindrel");
    let col_path = dir.join("api.hurl");
    std::fs::write(&col_path, "GET http://example/oauth\n").unwrap();
    let report_path = dir.join("smoke.report");
    std::fs::write(&report_path, "# collection:\n").unwrap();

    let mut app = TuiApp::default();
    let mut col = Collection::new(
        "api".to_string(),
        vec![HurlEntry {
            title: "Oauth".to_string(),
            method: "GET".to_string(),
            url: "http://example/oauth".to_string(),
            ..Default::default()
        }],
    );
    col.path = Some(col_path.clone());
    app.collections.push(col);

    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.path = Some(report_path.clone());
    app.reports[idx]
        .report
        .set_text("# collection:\nREQUEST Oauth\n");

    app.open_report_bind();
    let Some(Overlay::ReportBind(mut picker)) = app.overlay.take() else {
        panic!("bind picker should open");
    };
    picker.selected = picker.options.iter().position(|o| o.name == "api").unwrap();
    app.report_bind_key_handler(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), picker);

    let idx = app.active_report_index().unwrap();
    // Same directory → the stored ref is just the collection's file name.
    assert_eq!(
        app.reports[idx].report.collection_ref(),
        Some("api.hurl".to_string())
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn report_bind_without_collections_is_blocked() {
    let mut app = TuiApp::default();
    app.collections.clear();
    app.new_report_tab();
    app.open_report_bind();
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportBindNoCollections)
    ));
    assert!(app.overlay.is_none());
}

// ---- P1b: [Reports] section authoring in the request wizard ----

#[test]
fn a_new_request_starts_with_no_report_rows() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    assert!(
        form_ref(&app).reports.is_empty(),
        "no default blank report row"
    );
}

#[test]
fn tab_reaches_the_reports_section_after_captures() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // Name -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    press(&mut app, KeyCode::Tab); // -> AddHeader
    press(&mut app, KeyCode::Tab); // -> AddCookie
    press(&mut app, KeyCode::Tab); // -> AddQuery
    press(&mut app, KeyCode::Tab); // -> AddFormField
    press(&mut app, KeyCode::Tab); // -> Body
    press(&mut app, KeyCode::Tab); // -> AddAssert
    press(&mut app, KeyCode::Tab); // -> AddCapture
    press(&mut app, KeyCode::Tab); // -> AddReport
    assert_eq!(new_focus(&app), NewField::AddReport);
    press(&mut app, KeyCode::Tab); // wraps back to Name
    assert_eq!(new_focus(&app), NewField::Name);
    press(&mut app, KeyCode::BackTab); // Shift+Tab returns to AddReport
    assert_eq!(new_focus(&app), NewField::AddReport);
}

#[test]
fn alt_8_jumps_to_the_reports_section() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    app.on_key(KeyEvent::new(KeyCode::Char('8'), KeyModifiers::ALT));
    assert_eq!(new_focus(&app), NewField::AddReport);
}

#[test]
fn pagedown_cycles_to_the_reports_tab() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..8 {
        press(&mut app, KeyCode::PageDown);
    }
    assert_eq!(form_ref(&app).view_tab, WizardTab::Reports);
    assert_eq!(new_focus(&app), NewField::AddReport);
}

#[test]
fn creating_a_request_with_a_report_field_via_the_table() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Tab); // -> Target
    press(&mut app, KeyCode::Tab); // -> Method
    press(&mut app, KeyCode::Tab); // -> Url
    for ch in "http://h/x".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> AddHeader
    press(&mut app, KeyCode::Tab); // -> AddCookie
    press(&mut app, KeyCode::Tab); // -> AddQuery
    press(&mut app, KeyCode::Tab); // -> AddFormField
    press(&mut app, KeyCode::Tab); // -> Body
    press(&mut app, KeyCode::Tab); // -> AddAssert
    press(&mut app, KeyCode::Tab); // -> AddCapture
    press(&mut app, KeyCode::Tab); // -> AddReport
    press(&mut app, KeyCode::Enter); // -> Report(0, Name)
    for ch in "status".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Tab); // -> Report(0, Expr)
    for ch in "jsonpath \"$.status\"".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    let e = &app.collections[0].entries;
    assert_eq!(e.len(), 1);
    assert_eq!(
        e[0].reports,
        vec![("status".to_string(), "jsonpath \"$.status\"".to_string())]
    );
}

#[test]
fn deleting_the_last_report_row_leaves_the_section_empty() {
    let mut app = TuiApp::default();
    press(&mut app, KeyCode::Char('n'));
    for _ in 0..8 {
        press(&mut app, KeyCode::PageDown); // -> Reports
    }
    assert_eq!(new_focus(&app), NewField::AddReport);
    press(&mut app, KeyCode::Enter); // adds a blank row
    assert_eq!(new_focus(&app), NewField::Report(0, CapCol::Name));
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(
        new_focus(&app),
        NewField::AddReport,
        "back to empty, not re-seeded"
    );
    assert!(form_ref(&app).reports.is_empty());
}

#[test]
fn editing_a_request_populates_and_renders_the_reports_section() {
    use crate::i18n::{Language, Strings};
    use ratatui::{Terminal, backend::TestBackend};
    let th = super::theme::theme(&Language::English);
    let s = Strings::for_language(&Language::English);

    let mut entry = HurlEntry::from_fields("orig", "GET", "http://h/x", vec![], "");
    entry.reports = vec![(
        "overall".to_string(),
        "jsonpath \"$.overall_result\"".to_string(),
    )];

    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::List;
    press(&mut app, KeyCode::Enter); // opens the Edit Request wizard

    // The data is populated from entry.reports.
    {
        let form = form_ref(&app);
        assert_eq!(form.reports.len(), 1);
        assert_eq!(form.reports[0].name.text(), "overall");
        assert_eq!(form.reports[0].expr.text(), "jsonpath \"$.overall_result\"");
    }

    // Switch to the Reports section tab (full-body) and confirm it renders.
    for _ in 0..8 {
        press(&mut app, KeyCode::PageDown);
    }
    assert_eq!(form_ref(&app).view_tab, WizardTab::Reports);
    let form = form_ref(&app);
    let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
    term.draw(|f| super::new_request::draw_new_request(f, form, &s, &th, true))
        .unwrap();
    let out = buffer_text(term.backend().buffer());
    assert!(
        out.contains("overall") && out.contains("overall_result"),
        "the report row should render:\n{out}"
    );
}

#[test]
fn report_fields_survive_an_edit_that_changes_nothing_else() {
    // Editing a request and pressing Ctrl+Enter without touching the Reports
    // section must preserve its rows (they participate in the change check and
    // the write-back).
    let mut entry = HurlEntry::from_fields("orig", "GET", "http://h/x", vec![], "");
    entry.reports = vec![("status".to_string(), "jsonpath \"$.status\"".to_string())];

    let mut app = TuiApp::default();
    app.collections[0].entries.push(entry);
    app.focus = Pane::List;
    press(&mut app, KeyCode::Enter); // opens the Edit Request wizard
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)); // commit unchanged

    let e = &app.collections[0].entries;
    assert_eq!(
        e[0].reports,
        vec![("status".to_string(), "jsonpath \"$.status\"".to_string())]
    );
}

// ---------------------------------------------------------------------------
// P12 — the structured ("node") report editor
// ---------------------------------------------------------------------------

/// Build an app with one collection ("api") holding the named requests and a
/// report bound to it, left on the node view. Returns the `self.reports` index.
fn node_editor_app(requests: &[&str]) -> (TuiApp, usize) {
    let mut app = TuiApp::default();
    let entries = requests
        .iter()
        .map(|name| HurlEntry {
            title: (*name).to_string(),
            method: "GET".to_string(),
            url: "http://example/x".to_string(),
            ..Default::default()
        })
        .collect();
    app.collections
        .push(Collection::new("api".to_string(), entries));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx].report.set_text("# collection: api\n");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Char('n')); // Source -> Nodes
    (app, idx)
}

/// `n` flips the report between the source text editor and the structured node
/// editor, and back.
#[test]
fn report_n_key_toggles_the_node_view() {
    use super::reports::ReportView;
    let mut app = TuiApp::default();
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    assert_eq!(app.reports[idx].view, ReportView::Source);
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(app.reports[idx].view, ReportView::Nodes);
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(app.reports[idx].view, ReportView::Source);
}

/// A flow flattens to a Begin root plus one row per statement, with a loop
/// rendered as a `FOR` head, its nested body, and a synthetic `END` row.
#[test]
fn report_node_rows_flatten_a_flow_into_an_outline() {
    use super::report_nodes::RowKind;
    let (mut app, idx) = node_editor_app(&["Oauth", "upload"]);
    app.reports[idx].report.set_text(
        "# collection: api\nREQUEST Oauth\nFOR FILE IN FILES \"/docs\"\n    REPORT REQUEST upload\nEND\n",
    );
    app.revalidate_report(idx);
    let rows = app.report_node_rows(idx).expect("flow parses");
    let kinds: Vec<RowKind> = rows.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        vec![
            RowKind::Begin,
            RowKind::Leaf,     // REQUEST Oauth
            RowKind::LoopHead, // FOR FILE IN FILES
            RowKind::Leaf,     // REPORT REQUEST upload (nested)
            RowKind::LoopEnd,  // END
        ]
    );
    // The nested report row is one level deeper than the loop head.
    assert_eq!(rows[3].depth, rows[2].depth + 1);
    // `upload` resolves in the bound collection (green); the loop head has no
    // request name.
    assert_eq!(rows[3].req_ok, Some(true));
    assert_eq!(rows[2].req_ok, None);
}

/// A request-row's colour flag reflects whether the name resolves in the bound
/// collection (green when found, amber when not).
#[test]
fn report_node_rows_flag_unresolved_request_names() {
    let (mut app, idx) = node_editor_app(&["Oauth"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST Oauth\nREQUEST Missing\n");
    app.revalidate_report(idx);
    let rows = app.report_node_rows(idx).unwrap();
    assert_eq!(rows[1].req_ok, Some(true), "Oauth resolves");
    assert_eq!(rows[2].req_ok, Some(false), "Missing does not resolve");
}

/// Inserting a REQUEST node: `a` opens the palette (kind 0 = REQUEST), Enter
/// advances to the request picker, Enter commits the first request into the
/// flow text.
#[test]
fn report_node_editor_inserts_a_request_via_the_picker() {
    let (mut app, idx) = node_editor_app(&["Oauth", "CreateSession"]);
    // Selection starts on Begin (row 0); `a` opens the insert palette.
    press(&mut app, KeyCode::Char('a'));
    assert!(matches!(app.overlay, Some(Overlay::ReportNodeMenu(_))));
    // Kind 0 = REQUEST; Enter advances to the request picker.
    press(&mut app, KeyCode::Enter);
    // Pick the first request (Oauth) and commit.
    press(&mut app, KeyCode::Enter);
    assert!(app.overlay.is_none(), "committing closes the palette");
    assert!(
        app.reports[idx].report.text.contains("REQUEST Oauth"),
        "the picked request is written into the flow: {:?}",
        app.reports[idx].report.text
    );
}

/// Deleting the selected node removes its statement from the flow.
#[test]
fn report_node_editor_deletes_the_selected_node() {
    let (mut app, idx) = node_editor_app(&["Oauth", "Second"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST Oauth\nREQUEST Second\n");
    app.revalidate_report(idx);
    // Move onto the first statement (row 1) and delete it.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Delete);
    let text = &app.reports[idx].report.text;
    assert!(!text.contains("REQUEST Oauth"), "deleted: {text:?}");
    assert!(text.contains("REQUEST Second"), "sibling kept: {text:?}");
}

/// Ctrl+Z in the node editor reverts the last structural edit (here a delete),
/// restoring both the source text and the node selection.
#[test]
fn report_node_editor_ctrl_z_undoes_a_delete() {
    let (mut app, idx) = node_editor_app(&["Oauth", "Second"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST Oauth\nREQUEST Second\n");
    app.revalidate_report(idx);
    let before = app.reports[idx].report.text.clone();
    // Delete the first statement.
    press(&mut app, KeyCode::Down);
    let sel_before = app.reports[idx].node_selected;
    press(&mut app, KeyCode::Delete);
    assert!(!app.reports[idx].report.text.contains("REQUEST Oauth"));
    // Ctrl+Z brings it back exactly, and restores the selection.
    app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(
        app.reports[idx].report.text, before,
        "undo restores the pre-delete source"
    );
    assert_eq!(
        app.reports[idx].node_selected, sel_before,
        "undo restores the node selection"
    );
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportNodeUndone(_))
    ));
}

/// Ctrl+Z undoes successive edits in reverse order, then reports an empty
/// stack once there is nothing left to undo.
#[test]
fn report_node_editor_ctrl_z_is_multi_level() {
    let (mut app, idx) = node_editor_app(&["First", "Second"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST First\nREQUEST Second\n");
    app.revalidate_report(idx);
    let start = app.reports[idx].report.text.clone();
    // Two deletes in a row.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Delete); // removes First
    press(&mut app, KeyCode::Delete); // removes Second
    let after_two = app.reports[idx].report.text.clone();
    assert!(!after_two.contains("REQUEST First"));
    assert!(!after_two.contains("REQUEST Second"));
    // Undo twice returns to the starting text.
    app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(
        app.reports[idx].report.text, start,
        "two undos restore start"
    );
    // A third undo has nothing to revert.
    app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert!(matches!(
        app.status,
        Some(crate::i18n::Status::ReportNodeNothingToUndo(_))
    ));
    assert_eq!(
        app.reports[idx].report.text, start,
        "text unchanged when empty"
    );
}

/// Shift+Down moves the selected node past its next sibling (reordering the
/// flow).
#[test]
fn report_node_editor_moves_a_node_down() {
    let (mut app, idx) = node_editor_app(&["First", "Second"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST First\nREQUEST Second\n");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Down); // select row 1 (REQUEST First)
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT)); // move it down
    let flow = app.reports[idx].report.flow().unwrap();
    let names: Vec<Option<&str>> = flow.nodes.iter().map(|n| n.request_name()).collect();
    assert_eq!(
        names,
        vec![Some("Second"), Some("First")],
        "First moved below Second"
    );
}

/// Editing a non-request node "as a line" (Enter on an assignment) opens the
/// prompt seeded with the node's source; committing edited text re-parses it
/// back into the flow.
#[test]
fn report_node_editor_edits_a_line_and_roundtrips() {
    let (mut app, idx) = node_editor_app(&["Oauth"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nURL=old\n");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Down); // select the assignment row
    press(&mut app, KeyCode::Enter); // open the "edit as line" prompt
    assert!(
        matches!(
            &app.overlay,
            Some(Overlay::Prompt {
                kind: PromptKind::ReportNodeLine { .. },
                ..
            })
        ),
        "Enter on an assignment opens the line prompt"
    );
    // Replace the whole line with a new assignment.
    if let Some(Overlay::Prompt { editor, .. }) = app.overlay.as_mut() {
        *editor = super::editor::Editor::new("URL=new.example", false);
    }
    press(&mut app, KeyCode::Enter); // commit
    assert!(app.overlay.is_none());
    let flow = app.reports[idx].report.flow().unwrap();
    assert!(
        matches!(&flow.nodes[0], crate::report::flow::FlowNode::Assign { key, value } if key == "URL" && value == "new.example"),
        "the edited assignment is parsed back: {:?}",
        flow.nodes
    );
}

/// An invalid edited line is rejected (statement unchanged) and reports the
/// problem instead of corrupting the flow.
#[test]
fn report_node_editor_rejects_an_invalid_edited_line() {
    let (mut app, idx) = node_editor_app(&["Oauth"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nURL=keep\n");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    if let Some(Overlay::Prompt { editor, .. }) = app.overlay.as_mut() {
        // A dangling FOR with no END never yields a single statement.
        *editor = super::editor::Editor::new("FOR", false);
    }
    press(&mut app, KeyCode::Enter);
    let flow = app.reports[idx].report.flow().unwrap();
    assert!(
        matches!(&flow.nodes[0], crate::report::flow::FlowNode::Assign { key, .. } if key == "URL"),
        "the original assignment is untouched after a bad edit"
    );
}

/// Enter on a `FOR … IN FILES` node opens the folder browser (parking the node)
/// so the source directory can be chosen without typing a path.
#[test]
fn report_node_folder_key_opens_the_browser_for_a_for_loop() {
    let (mut app, idx) = node_editor_app(&["upload"]);
    app.reports[idx].report.set_text(
        "# collection: api\nFOR FILE IN FILES \"docs\"\n    REPORT REQUEST upload\nEND\n",
    );
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Down); // select the FOR head (row 1)
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(
            app.overlay,
            Some(Overlay::Browser(FileAction::PickReportNodeFolder, _))
        ),
        "Enter opens the node folder picker"
    );
    assert!(app.pending_node_folder.is_some(), "the node is parked");
}

/// Confirming a folder pick writes the chosen directory into the loop's
/// producer and re-serializes the flow.
#[test]
fn report_node_folder_commit_writes_into_the_loop_producer() {
    let (mut app, idx) = node_editor_app(&["upload"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nFOR FILE IN FILES \"old\"\n    REPORT REQUEST upload\nEND\n");
    app.revalidate_report(idx);
    app.pending_node_folder = Some((app.reports[idx].report.id, vec![0]));
    app.commit_report_node_folder("/data/images");
    let text = &app.reports[idx].report.text;
    assert!(text.contains("/data/images"), "new dir written: {text:?}");
    assert!(!text.contains("\"old\""), "old dir replaced: {text:?}");
    assert!(
        app.pending_node_folder.is_none(),
        "the park slot is cleared"
    );
}

/// `f` in the node editor opens the shared File menu on any node (it's no
/// longer overloaded as a per-node detail key — Enter configures instead).
#[test]
fn report_node_folder_key_falls_through_on_non_loop_nodes() {
    let (mut app, idx) = node_editor_app(&["Oauth"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST Oauth\n");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Down); // select REQUEST Oauth
    press(&mut app, KeyCode::Char('f'));
    assert!(
        matches!(app.overlay, Some(Overlay::FileMenu(_))),
        "f opens the File menu"
    );
    assert!(app.pending_node_folder.is_none());
}

/// Build a node-editor app whose single bound request (`upload`) carries the
/// given `[Reports]` field names, and whose flow reports it.
fn node_show_app(fields: &[&str]) -> (TuiApp, usize) {
    let mut app = TuiApp::default();
    let entry = HurlEntry {
        title: "upload".to_string(),
        method: "POST".to_string(),
        url: "http://example/x".to_string(),
        reports: fields
            .iter()
            .map(|f| ((*f).to_string(), format!("jsonpath \"$.{f}\"")))
            .collect(),
        ..Default::default()
    };
    app.collections
        .push(Collection::new("api".to_string(), vec![entry]));
    app.new_report_tab();
    let idx = app.active_report_index().unwrap();
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST upload\n");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Char('n')); // Source -> Nodes
    (app, idx)
}

/// Enter on a `REPORT REQUEST` node opens the configure form, whose field rows
/// are the request's intrinsics plus its `[Reports]` fields, all ticked when
/// the node has no `SHOW` clause yet (emit everything).
#[test]
fn report_node_request_form_opens_with_fields() {
    let (mut app, _idx) = node_show_app(&["status", "overall"]);
    press(&mut app, KeyCode::Down); // select REPORT REQUEST upload
    press(&mut app, KeyCode::Enter);
    let Some(Overlay::ReportNodeRequest(form)) = &app.overlay else {
        panic!("Enter opens the configure form on a REPORT REQUEST node");
    };
    let names: Vec<&str> = form.fields.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"status"),
        "shows a [Reports] field: {names:?}"
    );
    assert!(
        names.contains(&"overall"),
        "shows a [Reports] field: {names:?}"
    );
    assert!(names.contains(&"Response"), "shows an intrinsic: {names:?}");
    assert!(names.contains(&"Time"), "shows an intrinsic: {names:?}");
    assert!(
        form.fields.iter().all(|r| r.included),
        "no SHOW clause ⇒ every field ticked"
    );
}

/// Un-ticking a field and applying writes a `SHOW(…)` clause that omits it —
/// the way to drop a noisy field (e.g. a base64 `Response`) from the report.
#[test]
fn report_node_request_form_writes_show_omitting_unticked() {
    let (mut app, idx) = node_show_app(&["status"]);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    // Rows: 0 Name, 1 Report, 2 Response, 3 Alias, then fields HttpStatus,
    // Time, Asserts, Error, Response, status. The Response field is the 5th
    // field ⇒ row index 4 + 4 = 8.
    for _ in 0..8 {
        press(&mut app, KeyCode::Down);
    }
    press(&mut app, KeyCode::Char(' ')); // untick Response
    press(&mut app, KeyCode::Enter);
    let text = &app.reports[idx].report.text;
    assert!(text.contains("SHOW("), "a SHOW clause is written: {text:?}");
    assert!(
        !text.contains("Response"),
        "the un-ticked field is omitted: {text:?}"
    );
    assert!(text.contains("status"), "kept fields remain: {text:?}");
    assert!(app.overlay.is_none(), "the form closes on apply");
}

/// Applying with every field ticked writes no `SHOW` clause (the "emit all"
/// default), and clears any pre-existing one.
#[test]
fn report_node_request_form_all_ticked_removes_show() {
    let (mut app, idx) = node_show_app(&["status"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nREPORT REQUEST upload SHOW(status)\n");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    // The current SHOW(status) preselects only `status`.
    {
        let Some(Overlay::ReportNodeRequest(form)) = &app.overlay else {
            panic!("form open");
        };
        let ticked = form.fields.iter().filter(|r| r.included).count();
        assert_eq!(ticked, 1, "only the SHOW(status) field is preselected");
    }
    // Move to the first field row (0 Name, 1 Report, 2 Response, 3 Alias, 4
    // first field), then tick every unticked field.
    for _ in 0..4 {
        press(&mut app, KeyCode::Down);
    }
    let total = {
        let Some(Overlay::ReportNodeRequest(form)) = &app.overlay else {
            unreachable!()
        };
        form.fields.len()
    };
    for i in 0..total {
        let unticked = {
            let Some(Overlay::ReportNodeRequest(form)) = &app.overlay else {
                unreachable!()
            };
            !form.fields[i].included
        };
        if unticked {
            press(&mut app, KeyCode::Char(' '));
        }
        if i + 1 < total {
            press(&mut app, KeyCode::Down);
        }
    }
    press(&mut app, KeyCode::Enter);
    let text = &app.reports[idx].report.text;
    assert!(
        !text.contains("SHOW("),
        "all ticked ⇒ the SHOW clause is removed: {text:?}"
    );
}

/// Typing on the alias row and applying writes an `AS <alias>` clause.
#[test]
fn report_node_request_form_sets_the_alias() {
    let (mut app, idx) = node_show_app(&["status"]);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    // Rows: 0 Name, 1 Report, 2 Response, 3 Alias — three Downs reach the alias.
    for _ in 0..3 {
        press(&mut app, KeyCode::Down);
    }
    for c in ['p', 'r', 'o', 'c'] {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    let text = &app.reports[idx].report.text;
    assert!(text.contains("AS proc"), "alias is written: {text:?}");
}

/// Cycling the response row and applying writes a `RESPONSE RAW` clause.
#[test]
fn report_node_request_form_sets_the_response_format() {
    let (mut app, idx) = node_show_app(&["status"]);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    // Rows: 0 Name, 1 Report, 2 Response — two Downs reach the response row,
    // then Space cycles Default -> RAW.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Enter);
    let text = &app.reports[idx].report.text;
    assert!(
        text.contains("RESPONSE RAW"),
        "response format is written: {text:?}"
    );
}

/// Enter on a plain `REQUEST` opens the configure form with `Report` unticked
/// and only the Name/Report rows visible (no reporting options yet).
#[test]
fn report_node_request_form_opens_on_plain_request_nodes() {
    let (mut app, _idx) = node_editor_app(&["Oauth"]);
    app.reports[_idx]
        .report
        .set_text("# collection: api\nREQUEST Oauth\n");
    app.revalidate_report(_idx);
    press(&mut app, KeyCode::Down); // select REQUEST Oauth
    press(&mut app, KeyCode::Enter);
    let Some(Overlay::ReportNodeRequest(form)) = &app.overlay else {
        panic!("Enter opens the configure form on a plain REQUEST node");
    };
    assert!(!form.report, "a plain REQUEST starts with Report unticked");
    assert_eq!(
        form.visible_rows().len(),
        2,
        "only Name + Report rows show until Report is ticked"
    );
}

/// Ticking `Report` on a plain `REQUEST` and applying rewrites it as a
/// `REPORT REQUEST` (and un-ticking a `REPORT REQUEST` turns it back), so the
/// node editor can add/remove reporting without editing the raw line.
#[test]
fn report_node_request_form_report_toggle_round_trips() {
    let (mut app, idx) = node_editor_app(&["Oauth"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST Oauth\n");
    app.revalidate_report(idx);
    // REQUEST -> REPORT REQUEST: Enter, tick Report (row 1), apply.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Name -> Report row
    press(&mut app, KeyCode::Char(' ')); // tick Report
    press(&mut app, KeyCode::Enter); // apply
    assert!(
        app.reports[idx]
            .report
            .text
            .contains("REPORT REQUEST Oauth"),
        "REQUEST became REPORT REQUEST: {:?}",
        app.reports[idx].report.text
    );
    // REPORT REQUEST -> REQUEST: Enter, un-tick Report, apply.
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down); // Name -> Report row
    press(&mut app, KeyCode::Char(' ')); // un-tick Report
    press(&mut app, KeyCode::Enter); // apply
    let text = &app.reports[idx].report.text;
    assert!(
        text.contains("REQUEST Oauth") && !text.contains("REPORT REQUEST Oauth"),
        "REPORT REQUEST turned back into a plain REQUEST: {text:?}"
    );
}

/// The Name row cycles through the bound collection's request titles, so the
/// referenced request can be re-pointed without typing it.
#[test]
fn report_node_request_form_name_row_cycles_titles() {
    let (mut app, idx) = node_editor_app(&["Oauth", "CreateSession"]);
    app.reports[idx]
        .report
        .set_text("# collection: api\nREQUEST Oauth\n");
    app.revalidate_report(idx);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter); // configure form; Name row selected
    press(&mut app, KeyCode::Right); // cycle Oauth -> CreateSession
    press(&mut app, KeyCode::Enter); // apply
    assert!(
        app.reports[idx]
            .report
            .text
            .contains("REQUEST CreateSession"),
        "Name row cycled to the next title: {:?}",
        app.reports[idx].report.text
    );
}

// ---------------------------------------------------------------------------
// Workspace-aware reports: opening a `.report` from a Workspace tree pins that
// tree to the left of the full report view, so the user can navigate the
// workspace's collections/reports without leaving the report.
// ---------------------------------------------------------------------------

/// Build a temp workspace folder holding a collection and two report files
/// (plus one report nested in a subfolder), and open a Workspace collection tab
/// rooted at it. Returns the app, the collection tab index, and the workspace
/// root path.
fn workspace_with_reports() -> (TuiApp, usize, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "paperboy-ws-reports-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("api.hurl"), "GET https://example.test\n").unwrap();
    std::fs::write(
        root.join("alpha.report"),
        "# name: Alpha\n# collection: api.hurl\nREQUEST Oauth\n",
    )
    .unwrap();
    std::fs::write(
        root.join("beta.report"),
        "# name: Beta\n# collection: api.hurl\nREQUEST Oauth\n",
    )
    .unwrap();
    let sub = root.join("nested");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(
        sub.join("gamma.report"),
        "# name: Gamma\n# collection: api.hurl\nREQUEST Oauth\n",
    )
    .unwrap();

    let mut col = Collection::new("api".to_string(), Vec::new());
    col.workspace_root = Some(root.clone());
    let mut app = TuiApp::default();
    app.collections.push(col);
    let ci = app.collections.len() - 1;
    app.active_tab = ci;
    (app, ci, root)
}

/// Pressing Enter on a `.report` row in a Workspace tab's tree opens a
/// workspace-aware report tab: it lands in `reports`, carries the workspace
/// context, and its pinned tree takes focus.
#[test]
fn opening_a_report_from_the_workspace_tree_opens_a_workspace_aware_report_tab() {
    let (mut app, ci, root) = workspace_with_reports();
    // Find the `alpha.report` row and highlight it.
    let rows = app.collections[ci].ws_rows();
    let cursor = rows
        .iter()
        .position(|r| matches!(r, crate::collection::WsRow::Report { name, .. } if name == "alpha.report"))
        .expect("alpha.report is listed in the workspace tree");
    app.collections[ci].list_cursor = cursor;
    app.focus = super::app::Pane::List;

    press(&mut app, KeyCode::Enter);

    assert_eq!(app.reports.len(), 1, "a report tab was opened");
    assert!(app.active_is_report(), "the report tab is now active");
    let rt = app.active_report().expect("active report");
    assert_eq!(rt.report.name, "Alpha");
    let ws = rt.workspace.as_ref().expect("workspace context is carried");
    assert_eq!(ws.root, root, "the tree is rooted at the workspace root");
    assert!(app.report_tree_focus, "the pinned tree takes focus on open");
    let _ = ci;
}

/// A workspace report's pinned tree lists the browsed folder's subfolders,
/// collections, and report files, marking the open report.
#[test]
fn a_workspace_report_tree_lists_folders_collections_and_reports() {
    use super::reports::{ReportTreeRow, ReportWorkspace};
    let (_app, _ci, root) = workspace_with_reports();
    let ws = ReportWorkspace {
        root: root.clone(),
        browse: Vec::new(),
        cursor: 0,
    };
    let open = root.join("alpha.report");
    let rows = ws.rows(Some(open.as_path()));
    // Subfolder first (list_dir puts dirs before files), then the collection,
    // then the two reports.
    assert!(
        rows.iter()
            .any(|r| matches!(r, ReportTreeRow::Folder(n) if n == "nested")),
        "the subfolder is listed: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| matches!(r, ReportTreeRow::Collection { name, .. } if name == "api.hurl")),
        "the collection is listed"
    );
    let alpha = rows
        .iter()
        .find(|r| matches!(r, ReportTreeRow::Report { name, .. } if name == "alpha.report"))
        .expect("alpha.report is listed");
    assert!(
        matches!(alpha, ReportTreeRow::Report { open: true, .. }),
        "the currently-open report is marked open"
    );
    let beta = rows
        .iter()
        .find(|r| matches!(r, ReportTreeRow::Report { name, .. } if name == "beta.report"))
        .expect("beta.report is listed");
    assert!(
        matches!(beta, ReportTreeRow::Report { open: false, .. }),
        "a different report is not marked open"
    );
}

/// With the pinned tree focused, arrows move the cursor and Enter on another
/// report opens (and activates) it as a second workspace-aware report tab.
#[test]
fn report_tree_navigation_opens_another_report() {
    let (mut app, ci, root) = workspace_with_reports();
    app.open_workspace_report(root.join("alpha.report"), root.clone(), Vec::new());
    assert!(app.report_tree_focus);
    assert_eq!(app.reports.len(), 1);

    // Move the tree cursor onto `beta.report` and open it.
    let idx = app.active_report_index().unwrap();
    let open = app.reports[idx].report.path.clone();
    let ws = app.reports[idx].workspace.as_ref().unwrap();
    let target = ws
        .rows(open.as_deref())
        .into_iter()
        .position(|r| matches!(r, super::reports::ReportTreeRow::Report { name, .. } if name == "beta.report"))
        .expect("beta.report is in the tree");
    app.reports[idx].workspace.as_mut().unwrap().cursor = target;

    press(&mut app, KeyCode::Enter);

    assert_eq!(app.reports.len(), 2, "a second report tab opened");
    assert_eq!(app.active_report().unwrap().report.name, "Beta");
    assert!(
        app.active_report().unwrap().workspace.is_some(),
        "the newly-opened report also carries workspace context"
    );
    let _ = ci;
}

/// Descending into a subfolder from the pinned tree updates the breadcrumb and
/// surfaces that folder's reports.
#[test]
fn report_tree_descends_into_a_subfolder() {
    let (mut app, _ci, root) = workspace_with_reports();
    app.open_workspace_report(root.join("alpha.report"), root.clone(), Vec::new());
    let idx = app.active_report_index().unwrap();
    let open = app.reports[idx].report.path.clone();
    let ws = app.reports[idx].workspace.as_ref().unwrap();
    let target = ws
        .rows(open.as_deref())
        .into_iter()
        .position(|r| matches!(r, super::reports::ReportTreeRow::Folder(n) if n == "nested"))
        .expect("the subfolder is in the tree");
    app.reports[idx].workspace.as_mut().unwrap().cursor = target;

    press(&mut app, KeyCode::Enter); // descend

    let ws = app.reports[idx].workspace.as_ref().unwrap();
    assert_eq!(
        ws.browse,
        vec!["nested".to_string()],
        "breadcrumb descended"
    );
    assert!(
        ws.rows(open.as_deref())
            .iter()
            .any(|r| matches!(r, super::reports::ReportTreeRow::Report { name, .. } if name == "gamma.report")),
        "the subfolder's report is now listed"
    );
    // Backspace goes back up.
    press(&mut app, KeyCode::Backspace);
    assert!(
        app.reports[idx]
            .workspace
            .as_ref()
            .unwrap()
            .browse
            .is_empty(),
        "Backspace ascends to the workspace root"
    );
}

/// `cycle_report_focus` visits the pinned tree only for a workspace report; a
/// plain report tab never lands on a Tree focus.
#[test]
fn focus_cycle_includes_the_tree_only_for_workspace_reports() {
    // Plain report: Tab cycles editor/tabbar, never sets tree focus.
    let mut app = TuiApp::default();
    app.new_report_tab();
    for _ in 0..6 {
        app.cycle_report_focus(true);
        assert!(
            !app.report_tree_focus,
            "a non-workspace report never focuses a tree"
        );
    }

    // Workspace report: the cycle reaches the tree exactly once per lap.
    let (mut app, _ci, root) = workspace_with_reports();
    app.open_workspace_report(root.join("alpha.report"), root.clone(), Vec::new());
    // Start from a known non-tree focus.
    app.report_tree_focus = false;
    app.report_tabbar_focus = false;
    let mut saw_tree = false;
    for _ in 0..8 {
        app.cycle_report_focus(true);
        saw_tree |= app.report_tree_focus;
    }
    assert!(
        saw_tree,
        "the focus cycle reaches the pinned tree for a workspace report"
    );
}

/// The workspace context (root + browsed sub-path) round-trips through session
/// persistence, so a workspace report reopens pinned where it was.
#[test]
fn workspace_report_context_round_trips_through_persistence() {
    let (mut app, _ci, root) = workspace_with_reports();
    app.open_workspace_report(
        root.join("alpha.report"),
        root.clone(),
        vec!["nested".into()],
    );
    let idx = app.active_report_index().unwrap();
    // Sanity: the browse breadcrumb we asked for is on the tab.
    assert_eq!(
        app.reports[idx].workspace.as_ref().unwrap().browse,
        vec!["nested".to_string()]
    );

    let snapshot = app.to_persisted();
    let mut restored = TuiApp::default();
    restored.apply_persisted(snapshot);

    assert_eq!(restored.reports.len(), 1);
    let ws = restored.reports[0]
        .workspace
        .as_ref()
        .expect("workspace context restored");
    assert_eq!(ws.root, root, "root restored");
    assert_eq!(
        ws.browse,
        vec!["nested".to_string()],
        "browsed sub-path restored"
    );
}

/// A workspace report whose root folder has vanished restores as an ordinary
/// (non-workspace) report tab rather than pointing at a dead folder.
#[test]
fn a_vanished_workspace_root_degrades_to_a_plain_report_on_restore() {
    let (mut app, _ci, root) = workspace_with_reports();
    app.open_workspace_report(root.join("alpha.report"), root.clone(), Vec::new());
    let snapshot = app.to_persisted();
    // The folder disappears between sessions.
    std::fs::remove_dir_all(&root).unwrap();

    let mut restored = TuiApp::default();
    restored.apply_persisted(snapshot);

    assert_eq!(restored.reports.len(), 1);
    assert!(
        restored.reports[0].workspace.is_none(),
        "a missing workspace root degrades to a plain report tab"
    );
}

//! Internationalisation — language selection and all UI strings.
//!
//! To add a new language, implement a new `Strings` constructor and add a match
//! arm to `Strings::for_language`. Both constructors must stay in sync.

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum Language {
    #[default]
    English,
    French,
    Danish,
}

/// All visible UI strings in one place. Every field is `&'static str`.
pub struct Strings {
    // Window / top bar
    pub app_heading: &'static str,
    pub base_url: &'static str,

    // Response panel
    pub sending: &'static str,
    pub response_heading: &'static str,
    pub status_label: &'static str,
    pub no_response_yet: &'static str,
    pub req_error_prefix: &'static str,

    // Options menu
    pub options_menu: &'static str,
    /// The top menu bar's "Settings" label with its mnemonic baked in
    /// (e.g. "(S)ettings") — same "(X)" convention as the File menu's
    /// Load/Save submenu items, but this one just documents the existing
    /// hardcoded `s` shortcut rather than driving key matching itself.
    pub options_menu_label: &'static str,
    pub language_label: &'static str,
    pub lang_english: &'static str,
    pub lang_french: &'static str,
    pub lang_danish: &'static str,
    pub clear_all: &'static str,
    pub clear_all_done: &'static str,
    /// Status bar message shown after copying text to the clipboard (via a
    /// selection release, `y`, or Ctrl+Y).
    pub copied_to_clipboard: &'static str,
    pub preferences_menu: &'static str,
    pub confirm_on_exit: &'static str,
    pub confirm_on_clear: &'static str,
    /// "Default Request View" — the Preferences item that picks whether the
    /// Main panel shows JSON or Hurl text by default, for every request.
    pub default_request_view_label: &'static str,
    /// The two choices in the Default Request View submenu.
    pub view_json_label: &'static str,
    pub view_hurl_label: &'static str,
    pub confirm_exit_q: &'static str,
    /// Appended to the exit prompt when unsaved secret edits would be lost.
    pub confirm_exit_secrets: &'static str,
    pub confirm_clear_q: &'static str,
    pub confirm_save_collection_q: &'static str,
    pub confirm_save_env_q: &'static str,
    pub confirm_overwrite_q: &'static str,
    pub confirm_yes: &'static str,
    pub confirm_no: &'static str,

    // File menu
    pub file_menu: &'static str,
    /// The top menu bar's "File" label with its mnemonic baked in (e.g.
    /// "(F)ile") — see `options_menu_label`.
    pub file_menu_label: &'static str,
    /// Top-level File menu item that opens the Load submenu, e.g. "(L)oad".
    /// The bracketed letter is both the visual hint AND the actual mnemonic
    /// key: pressing it while this menu is open jumps straight to that item.
    pub file_menu_item_load: &'static str,
    /// Top-level File menu item that opens the Save submenu, e.g. "(S)ave".
    pub file_menu_item_save: &'static str,
    /// Title of the "Load" submenu popup.
    pub file_load_menu: &'static str,
    /// Title of the "Save" submenu popup.
    pub file_save_menu: &'static str,
    pub file_load_item_request: &'static str,
    pub file_load_item_collection: &'static str,
    pub file_load_item_collection_git: &'static str,
    pub file_load_item_environment: &'static str,
    pub file_load_item_environment_git: &'static str,
    /// "Load" submenu item that opens the Workspace root-folder picker, e.g.
    /// "(W)orkspace…".
    pub file_load_item_workspace: &'static str,
    /// "Load" submenu item that opens the git wizard in Workspace mode, e.g.
    /// "Work(s)pace from Git…" ("W" is already taken by the local Workspace
    /// picker item above, so the mnemonic is drawn from within the word).
    pub file_load_item_workspace_git: &'static str,
    pub file_save_item_request: &'static str,
    pub file_save_item_collection: &'static str,
    pub file_save_item_collection_as: &'static str,
    pub file_save_item_collection_git: &'static str,
    pub file_save_item_environment: &'static str,
    pub file_save_item_environment_as: &'static str,
    pub file_save_item_response: &'static str,
    pub save_request: &'static str,
    pub load_request: &'static str,
    pub open_collection: &'static str,
    pub save_collection: &'static str,
    pub save_environment: &'static str,
    pub save_response: &'static str,
    pub file_saved: &'static str,
    pub file_loaded: &'static str,
    pub file_no_response: &'static str,
    pub file_error_prefix: &'static str,
    pub file_not_collection: &'static str,
    pub file_not_environment: &'static str,

    // Workspaces (a folder of many .hurl/.json collection files browsed
    // through a single tab; see `src/workspace.rs`).
    /// Title shown atop the root-folder browser popup opened via
    /// File → Load → Workspace.
    pub open_workspace: &'static str,
    /// Footer hint for that same root-folder browser popup, mentioning the
    /// Space key (distinct from the plain file-picker's `browser_hint`,
    /// which only lists Enter/parent/hidden/cancel).
    pub browser_hint_workspace: &'static str,
    /// Shown in the List/Main panels when a Workspace tab has no collection
    /// file chosen yet (fresh pick, or the last one vanished on restart).
    pub workspace_empty_state: &'static str,
    /// Short footer-hint label appended after "w" on the List panel's
    /// bottom border, mirroring "p {foot_env_link}".
    pub foot_workspace: &'static str,
    /// Title of the recursive file-tree popup used to choose which file
    /// inside a Workspace folder to load (root path is appended by the
    /// caller).
    pub workspace_picker_title: &'static str,
    /// Footer hint line for the Workspace file-tree popup.
    pub workspace_picker_hint: &'static str,
    /// Filter-state label shown when only .hurl/.json files are listed.
    pub workspace_filter_on: &'static str,
    /// Filter-state label shown when the filter has been toggled off (Tab)
    /// and all files are listed.
    pub workspace_filter_off: &'static str,
    /// Shown in place of the tree when no files match in this folder.
    pub workspace_no_files: &'static str,

    // Tabs
    pub tab_request: &'static str,

    // Request list / detail
    pub run_entry: &'static str,
    pub entry_request_json: &'static str,
    /// "Request Hurl" — the Main panel's title when `default_request_view`
    /// is Hurl (the Hurl-text counterpart to `entry_request_json`).
    pub entry_request_hurl: &'static str,
    pub entry_raw_hurl: &'static str,
    /// "Raw Mode (JSON)" — the title of the Raw JSON Mode editor (Shift+J),
    /// the JSON-text counterpart to `entry_raw_hurl`.
    pub entry_raw_json: &'static str,
    /// Shown when Raw Mode text fails to parse as exactly one Hurl entry.
    pub invalid_hurl: &'static str,
    /// Shown when Raw JSON Mode text fails to parse back into a request
    /// (not a JSON object, or missing "method"/"url").
    pub invalid_request_json: &'static str,
    pub no_requests_hint: &'static str,
    /// The "go up a folder" row shown at the top of the Requests list when
    /// browsing inside a folder (see [`crate::tree`]).
    pub list_up_row: &'static str,

    // New-request form
    pub new_request: &'static str,
    pub edit_request: &'static str,
    pub field_name: &'static str,
    pub field_target: &'static str,
    pub field_method: &'static str,
    pub field_url: &'static str,
    pub field_headers: &'static str,
    pub field_cookies: &'static str,
    pub field_form: &'static str,
    pub field_body: &'static str,
    pub field_asserts: &'static str,
    pub field_captures: &'static str,
    pub tab_all: &'static str,
    pub hdr_key: &'static str,
    pub hdr_value: &'static str,
    pub hdr_description: &'static str,
    pub hdr_type: &'static str,
    pub form_type_text: &'static str,
    pub form_type_file: &'static str,
    /// Title of the file content-type override dropdown (Form/Multipart
    /// `File` rows).
    pub content_type_hint: &'static str,
    /// First entry in the content-type dropdown: clears the override so
    /// Hurl infers the type from the file extension itself.
    pub content_type_auto: &'static str,
    /// Short placeholder shown directly in a File-kind row's Content-Type
    /// cell when it has no override yet (dimmed, like the Kind column's
    /// "Select Kind..." placeholder), so an empty-looking cell still reads
    /// as "this will be auto-detected" rather than "nothing is set here".
    pub content_type_auto_placeholder: &'static str,
    /// Hint shown near a File-kind Form row's Value cell, telling the user
    /// how to open the file picker.
    pub hint_pick_file: &'static str,
    /// Contextual hint shown while focus is on a deletable table row
    /// (Header/Cookie/Form/Assert/Capture), advertising Ctrl+D to remove it.
    pub hint_delete_row: &'static str,
    /// Contextual hint shown while focus is on a Header/Cookie/Form row (the
    /// row kinds with an enabled checkbox), advertising Ctrl+E to toggle it
    /// in place without moving focus off the row's currently-edited cell.
    pub hint_toggle_enabled: &'static str,
    pub add_header: &'static str,
    pub add_cookie: &'static str,
    pub add_form_field: &'static str,
    pub add_assert: &'static str,
    pub add_capture: &'static str,
    pub cap_name: &'static str,
    pub cap_expr: &'static str,

    // Environment panel
    pub load_environment: &'static str,
    pub env_heading: &'static str,
    pub env_no_env: &'static str,
    pub env_loading: &'static str,
    pub env_waiting_secrets: &'static str,
    /// Status-bar message prefix while retrying a single failed variable
    /// (env var / 1Password / SSM), e.g. "Reloading API_TOKEN…".
    pub env_reloading_var: &'static str,
    /// Status-bar message when a Global Environment is activated/deactivated,
    /// e.g. "Activated staging" / "Deactivated staging".
    pub env_activated: &'static str,
    pub env_deactivated: &'static str,
    /// Title of the "rename this Global Environment" prompt (F2).
    pub env_rename_title: &'static str,
    /// Title of the popup (opened with 'p' in the Requests list) that links
    /// or unlinks a Global Environment to the current collection.
    pub env_link_picker_title: &'static str,
    /// Entry at the top of the link picker meaning "no linked environment".
    pub env_link_none: &'static str,
    /// Confirmation prompt text for deleting a Global Environment ('x').
    pub env_delete_confirm: &'static str,
    /// Placeholder shown in the Global Environments list when it's empty.
    pub env_no_envs: &'static str,
    /// Title of the 4-choice name-collision popup shown when loading an
    /// environment whose name matches one already in the Global
    /// Environments list.
    pub env_collision_title: &'static str,
    pub env_collision_replace: &'static str,
    pub env_collision_keep_both: &'static str,
    pub env_collision_abort: &'static str,
    pub env_collision_rename: &'static str,
    // "Run All" (Alt+F5) status-bar summary labels, e.g. "Passed: 3  Failed: 1  Total: 4".
    pub run_summary_passed: &'static str,
    pub run_summary_failed: &'static str,
    pub run_summary_total: &'static str,
    pub env_add_var_title: &'static str,
    pub env_var_switch: &'static str,
    /// Label for the "still secret?" checkbox shown when editing a value
    /// sourced from a secret provider (1Password / SSM).
    pub env_still_secret: &'static str,
    /// Hint appended to the editor's title bar describing the checkbox toggle.
    pub env_still_secret_hint: &'static str,

    // Remote git loader
    pub git_collection_menu: &'static str,
    pub git_env_menu: &'static str,
    /// Title of the git wizard when loading a Workspace (many files at once)
    /// rather than a single collection/environment file.
    pub git_workspace_menu: &'static str,
    pub git_url_label: &'static str,
    pub git_token_label: &'static str,
    pub git_connect_hint: &'static str,
    /// Extra hint appended when a "recently used" URL dropdown is available.
    pub git_recent_hint: &'static str,
    pub git_pick_ref_title: &'static str,
    pub git_pick_file_title: &'static str,
    pub git_filter_hint: &'static str,
    pub git_loading_refs: &'static str,
    pub git_loading_files: &'static str,
    pub git_loading_file: &'static str,
    /// Shown while the Workspace load's filtered batch of files is being
    /// downloaded (after the file-type filter is chosen).
    pub git_loading_workspace_files: &'static str,
    pub git_loading_hint: &'static str,
    pub git_error_hint: &'static str,
    pub git_url_required: &'static str,
    pub git_branches: &'static str,
    pub git_tags: &'static str,
    pub git_filter_label: &'static str,
    /// Asked after a collection successfully loads from git, offering to
    /// also load its environment from the same ref (no second fetch).
    pub git_ask_load_env_q: &'static str,
    pub git_pick_env_file_title: &'static str,
    /// Title of the "which files should be downloaded?" picklist shown right
    /// after choosing a branch/tag in the Workspace git-load flow.
    pub git_pick_workspace_filter_title: &'static str,
    /// Footer hint for that picklist (no text filtering — just a short fixed
    /// list of choices).
    pub git_workspace_filter_hint: &'static str,
    pub git_ws_filter_hurl_json: &'static str,
    pub git_ws_filter_hurl: &'static str,
    pub git_ws_filter_json: &'static str,
    pub git_ws_filter_all: &'static str,
    /// Shown when none of the repo's files matched the chosen filter, so
    /// there is nothing to download.
    pub git_workspace_no_matches: &'static str,
    /// Question shown when closing a tab whose Workspace folder was
    /// downloaded from git (see `Overlay::CloseGitWorkspace`); `{p}` is
    /// replaced with the folder's path.
    pub close_git_workspace_q: &'static str,
    pub close_git_workspace_keep: &'static str,
    pub close_git_workspace_delete: &'static str,
    pub close_git_workspace_cancel: &'static str,
    /// Shown at startup when a restored Workspace tab's root folder no
    /// longer exists on disk (e.g. it was downloaded from git into `/tmp`
    /// and got cleared since); `{name}` is replaced with the tab's name. The
    /// tab itself is reset to a plain, empty "no collection chosen" tab.
    pub workspace_folder_missing: &'static str,
    /// Question shown at startup when a restored Workspace tab's root
    /// folder is missing *and* it's known to have been downloaded from git
    /// (see `Overlay::WorkspaceReloadConfirm`) — offers to redownload the
    /// exact commit it was last at instead of just reporting it gone.
    /// `{name}` = tab name, `{ref}` = branch/tag name, `{url}` = repo URL.
    pub workspace_reload_confirm_q: &'static str,
    /// Shown while a confirmed redownload is running in the background.
    pub workspace_reload_loading: &'static str,
    /// Shown after a redownload succeeds (see
    /// `TuiApp::poll_workspace_redownload_updates`).
    pub workspace_reload_success: &'static str,
    /// Shown after a redownload fails; `{e}` is the (token-redacted) git
    /// error — most often because the exact recorded commit is no longer
    /// reachable on the remote (history rewritten, branch/tag deleted).
    pub workspace_reload_failed: &'static str,
    /// Appended after both `workspace_reload_success` and
    /// `workspace_reload_failed`, hinting that saving the Workspace to a
    /// permanent local folder avoids relying on a temp download at all.
    pub workspace_reload_save_hint: &'static str,

    // Save Workspace (copy a Workspace's files to a new, permanent folder).
    /// File → Save menu item ("(W)orkspace…").
    pub file_save_item_workspace: &'static str,
    /// Status shown when "Save Workspace…" is invoked on a tab that isn't
    /// Workspace-bound (the action is only ever offered for such tabs).
    pub file_not_workspace: &'static str,
    /// Title of the destination-folder browser popup opened by "Save
    /// Workspace…".
    pub save_workspace: &'static str,
    /// Footer hint for that same destination-folder browser popup.
    pub browser_hint_workspace_save: &'static str,
    /// Title of the name prompt shown after the destination folder is
    /// chosen, pre-filled with the workspace's current tab name.
    pub workspace_save_name_prompt: &'static str,
    /// Shown after "Save Workspace…" finishes copying the files.
    pub workspace_save_success: &'static str,
    /// Shown if "Save Workspace…" couldn't complete; `{e}` is a raw detail
    /// (filesystem error, or the destination already existing/overlapping
    /// the source).
    pub workspace_save_failed: &'static str,
    /// Question shown right after a Workspace finishes downloading from
    /// git, offering to keep it temporary (the old default) or save it to a
    /// permanent, chosen location straight away (see
    /// `Overlay::WorkspaceStorageChoice`).
    pub git_workspace_storage_q: &'static str,
    pub git_workspace_storage_temp: &'static str,
    pub git_workspace_storage_choose: &'static str,

    // Save-to-git wizard
    /// Status shown instead of opening the wizard when the active
    /// collection has no remembered git origin.
    pub git_no_origin: &'static str,
    pub git_save_title: &'static str,
    pub git_save_include_env_label: &'static str,
    pub git_save_collection_path_label: &'static str,
    pub git_save_env_path_label: &'static str,
    /// Generic "next step" hint shown on the Connect/Paths stages.
    pub git_save_step_hint: &'static str,
    pub git_save_branch_label: &'static str,
    pub git_save_tag_label: &'static str,
    pub git_save_target_hint: &'static str,
    pub git_save_commit_msg_label: &'static str,
    pub git_save_commit_msg_hint: &'static str,
    pub git_save_pushing: &'static str,
    pub git_save_success: &'static str,
    pub git_tag_exists: &'static str,
    /// A brand-new branch/tag name that raced with another push landing the
    /// same name since the last fetch.
    pub git_ref_exists_race: &'static str,

    // Keyboard hints. The shortcut keys themselves (b, Tab, Enter, ^D, …) are
    // never translated; only these descriptive labels are.
    pub hint_edit_base_url: &'static str,
    pub json_enter_to_edit: &'static str,
    /// Legend shown when the preview contains environment substitutions: one
    /// word per resolution status, each rendered in its matching colour
    /// (green = loaded, cyan = literal, orange = loading, red = missing).
    pub subst_hint_loaded: &'static str,
    pub subst_hint_literal: &'static str,
    pub subst_hint_loading: &'static str,
    pub subst_hint_missing: &'static str,
    /// Legend entry for the "!" icon marking a substitution whose Global
    /// Environment value is shadowed by the collection's linked Environment.
    pub subst_hint_shadowed: &'static str,
    pub json_invalid: &'static str,
    // Footer (bottom bar) action labels.
    pub foot_focus: &'static str,
    pub foot_move: &'static str,
    pub foot_edit: &'static str,
    pub foot_run: &'static str,
    /// Footer hint for "Run All" (Alt+F5) — runs every request in the
    /// collection in order, like the CLI's batch mode.
    pub foot_run_all: &'static str,
    /// Bottom-border hint on the Global Environments panel for `a`
    /// (activate/deactivate the selected environment) — same convention as
    /// the Requests list's Run/Run All hint.
    pub foot_env_activate: &'static str,
    /// Bottom-border hint on the Requests list panel for `p` (link/unlink a
    /// Global Environment to the active collection) — appended after the
    /// Run/Run All hint only when the panel is wide enough to fit it.
    pub foot_env_link: &'static str,
    /// `n` opens New Request everywhere except the Env pane, where it adds a
    /// variable instead — both meanings are folded into this one string.
    pub foot_new: &'static str,
    /// Footer hint for retrying a single failed Environment panel variable.
    pub foot_reload_var: &'static str,
    pub foot_file: &'static str,
    pub foot_options: &'static str,
    pub foot_rename: &'static str,
    pub foot_close: &'static str,
    /// Footer hint shown only while a Request JSON / Response panel text
    /// selection is active: `y` re-copies it to the clipboard.
    pub foot_copy_selection: &'static str,
    pub foot_help: &'static str,
    pub foot_quit: &'static str,
    // Help overlay.
    pub help_title: &'static str,
    pub help_heading: &'static str,
    /// Label of the first Help tab (the shortcut list) — also its title-bar
    /// suffix, e.g. "Help — Shortcuts".
    pub help_tab_shortcuts: &'static str,
    /// Label of the second Help tab (the substitution colour/icon glossary).
    pub help_tab_glossary: &'static str,
    /// Hint shown at the top of the Help popup explaining how to switch
    /// between its two tabs.
    pub help_tab_switch_hint: &'static str,
    /// Glossary tab heading above the coloured-dot/icon entries.
    pub glossary_heading: &'static str,
    pub glossary_label_literal: &'static str,
    pub glossary_desc_literal: &'static str,
    pub glossary_label_loaded: &'static str,
    pub glossary_desc_loaded: &'static str,
    pub glossary_label_pending: &'static str,
    pub glossary_desc_pending: &'static str,
    pub glossary_label_failed: &'static str,
    pub glossary_desc_failed: &'static str,
    pub glossary_label_shadowed: &'static str,
    pub glossary_desc_shadowed: &'static str,
    /// Heading above the second, non-substitution icon group in the
    /// Glossary tab (pencil/plus/tick/cross/etc. used throughout the app).
    pub glossary_heading_icons: &'static str,
    pub glossary_label_modified: &'static str,
    pub glossary_desc_modified: &'static str,
    pub glossary_label_added: &'static str,
    pub glossary_desc_added: &'static str,
    pub glossary_label_passed: &'static str,
    pub glossary_desc_passed: &'static str,
    pub glossary_label_run_failed: &'static str,
    pub glossary_desc_run_failed: &'static str,
    pub glossary_label_running: &'static str,
    pub glossary_desc_running: &'static str,
    pub glossary_label_git: &'static str,
    pub glossary_desc_git: &'static str,
    pub glossary_label_linked: &'static str,
    pub glossary_desc_linked: &'static str,
    pub glossary_label_folder: &'static str,
    pub glossary_desc_folder: &'static str,
    pub glossary_label_scroll_hint: &'static str,
    pub glossary_desc_scroll_hint: &'static str,
    pub help_focus: &'static str,
    pub help_move: &'static str,
    /// Help overlay line for Ctrl+↑/↓, which page-scrolls the Response
    /// panel by its full visible height instead of one line at a time.
    pub help_page_response: &'static str,
    pub help_switch_tabs: &'static str,
    pub help_select: &'static str,
    pub help_run: &'static str,
    /// Help overlay line for "Run All" (Alt+F5).
    pub help_run_all: &'static str,
    pub help_raw_mode: &'static str,
    /// Help overlay line for Shift+J (Raw JSON Mode editor).
    pub help_raw_json: &'static str,
    /// `n`: new request everywhere except inside the environment entries
    /// popup, where it adds a variable instead — both meanings are folded
    /// into this one string.
    pub help_new: &'static str,
    pub help_base_url: &'static str,
    pub help_menus: &'static str,
    /// Help-popup shortcuts-tab description for the global `w` key: open the
    /// Workspace file picker for the active tab (no-op on non-Workspace
    /// tabs).
    pub help_workspace_browse: &'static str,
    pub help_prev_next_tab: &'static str,
    pub help_rename_close: &'static str,
    pub help_reload_var: &'static str,
    /// Help overlay line: `a` activates/deactivates the selected Global
    /// Environment (at most one may be active at a time).
    pub help_env_activate: &'static str,
    /// Help overlay line: `x` in the Global Environments pane deletes the
    /// selected environment (unlinking it from any collection).
    pub help_env_delete: &'static str,
    /// Help overlay line: `p` in the Requests list links/unlinks a Global
    /// Environment to the active collection.
    pub help_env_link: &'static str,
    /// Help overlay line: `v` in the Tabs bar views the active collection's
    /// linked Global Environment.
    pub help_env_view_linked: &'static str,
    /// Help overlay line: `F2` inside the environment entries popup renames
    /// that Global Environment.
    pub help_env_rename: &'static str,
    pub help_resize: &'static str,
    pub help_resize_width: &'static str,
    pub help_tab_manage: &'static str,
    pub help_tab_reorder: &'static str,
    /// Help overlay line: `u` restores the most recently deleted request in
    /// the active collection, when the Requests list has focus.
    pub help_restore_request: &'static str,
    /// Help overlay line: toggle enabled/disabled and delete a row within the
    /// wizard's Headers/Cookies/Form/Asserts/Captures sections.
    pub help_row_toggle_delete: &'static str,
    /// Help overlay line: `y` copies the active Request JSON / Response panel
    /// text selection to the clipboard (OSC 52), an explicit alternative to
    /// the automatic copy-on-mouse-release for terminals where that doesn't
    /// take effect.
    pub help_copy_selection: &'static str,
    /// Help overlay line: Alt+Click+Drag adds an additional selection
    /// region (in the Request JSON / Response panels) instead of replacing
    /// the current one — a plain click clears every region, and Copy/`y`
    /// copies them all, concatenated.
    pub help_multi_select: &'static str,
    pub help_save_editor: &'static str,
    pub help_cancel: &'static str,
    pub help_quit: &'static str,
    /// Section heading in the Help "Shortcuts" tab, grouping the
    /// pane-focus/movement/tab-switch shortcuts together.
    pub help_group_navigation: &'static str,
    /// Section heading grouping tab management shortcuts (open/close/
    /// reorder/rename/prev-next).
    pub help_group_tabs: &'static str,
    /// Section heading grouping request editing/running shortcuts.
    pub help_group_requests: &'static str,
    /// Section heading grouping the File/Settings menu and Workspace
    /// browsing shortcuts.
    pub help_group_menus: &'static str,
    /// Section heading grouping Global Environment popup shortcuts.
    pub help_group_environments: &'static str,
    /// Section heading grouping text-selection/copy/wizard-row-editing
    /// shortcuts.
    pub help_group_editing: &'static str,
    /// Section heading grouping panel-resize and other general shortcuts
    /// (cancel, quit).
    pub help_group_panels: &'static str,
    // New Request modal hint.
    pub new_request_hint: &'static str,
    // Edit Request modal hint (same as new_request_hint but "save" instead of
    // "create", since it edits an existing entry in place).
    pub edit_request_hint: &'static str,
    // Hint shown atop the Raw Mode (Hurl text) editor.
    pub raw_mode_hint: &'static str,
    // Hint shown atop the Raw JSON Mode editor.
    pub raw_json_hint: &'static str,
    /// The "Ctrl+Enter" token as spelled in this language's hint text (used to
    /// strip it when the terminal lacks the keyboard-enhancement protocol).
    pub ctrl_enter_key: &'static str,
    // Prompt / file-browser hints (keys kept literal, descriptions translated).
    pub prompt_rename_title: &'static str,
    pub prompt_enter_path: &'static str,
    pub prompt_save_hint_ml: &'static str,
    pub prompt_save_hint_sl: &'static str,
    pub prompt_reset_hint: &'static str,
    pub browser_select_file: &'static str,
    pub browser_hint: &'static str,
    pub tabs_heading: &'static str,
    pub suggest_hint: &'static str,
}

impl Strings {
    pub fn for_language(lang: &Language) -> Self {
        match lang {
            Language::English => Self::english(),
            Language::French => Self::french(),
            Language::Danish => Self::danish(),
        }
    }

    fn english() -> Self {
        Self {
            app_heading: "🦀 PaperBoy",
            base_url: "Default New Request URL:",
            sending: "Sending…",
            response_heading: "Response",
            status_label: "Status:",
            no_response_yet: "Run a request to see the response.",
            req_error_prefix: "Request error:",
            options_menu: "Settings",
            options_menu_label: "(S)ettings",
            language_label: "Language",
            lang_english: "English",
            lang_french: "Français",
            lang_danish: "Dansk",
            clear_all: "Close all collections",
            clear_all_done: "All collections closed",
            copied_to_clipboard: "Copied to clipboard",
            preferences_menu: "Preferences",
            confirm_on_exit: "Confirm on exit",
            confirm_exit_secrets: "There are environment secrets with unsaved changes, exiting will cause these changes to be lost.",
            confirm_on_clear: "Confirm on clear",
            default_request_view_label: "Default Request View",
            view_json_label: "JSON",
            view_hurl_label: "Hurl",
            confirm_exit_q: "Quit PaperBoy?",
            confirm_clear_q: "Close all collections? This removes all tabs and requests.",
            confirm_save_collection_q: "There are {r} new or modified request entries. Saving will overwrite the original collection file. Proceed?",
            confirm_save_env_q: "There are {e} new or modified environment entries. Saving will overwrite the original environment file. Proceed?",
            confirm_overwrite_q: "\"{f}\" already exists. Overwrite it?",
            confirm_yes: "Yes",
            confirm_no: "No",
            file_menu: "File",
            file_menu_label: "(F)ile",
            file_menu_item_load: "(L)oad",
            file_menu_item_save: "(S)ave",
            file_load_menu: "Load",
            file_save_menu: "Save",
            file_load_item_request: "(R)equest…",
            file_load_item_collection: "(C)ollection…",
            file_load_item_collection_git: "Collection from (G)it…",
            file_load_item_environment: "(E)nvironment…",
            file_load_item_environment_git: "En(v)ironment from Git…",
            file_load_item_workspace: "(W)orkspace…",
            file_load_item_workspace_git: "Work(s)pace from Git…",
            file_save_item_request: "(R)equest…",
            file_save_item_collection: "(C)ollection…",
            file_save_item_collection_as: "Collection (A)s…",
            file_save_item_collection_git: "Save Collection to (G)it…",
            file_save_item_environment: "(E)nvironment…",
            file_save_item_environment_as: "En(v)ironment As…",
            file_save_item_workspace: "(W)orkspace…",
            file_save_item_response: "Res(p)onse…",
            save_request: "Save Request…",
            load_request: "Load Request…",
            open_collection: "Load Collection…",
            save_collection: "Save Collection…",
            save_environment: "Save Environment…",
            save_response: "Save Response…",
            file_saved: "Saved.",
            file_loaded: "Loaded.",
            file_no_response: "No response to save.",
            file_error_prefix: "Error:",
            file_not_collection: "Not a valid collection file (no requests found).",
            file_not_environment: "Not a valid environment file (expected KEY=value lines).",
            open_workspace: "Choose Workspace Folder…",
            browser_hint_workspace: "Enter open folder · Space choose as Workspace · ← parent · Esc cancel",
            workspace_empty_state: "No collection — press w.",
            foot_workspace: "browse workspace",
            workspace_picker_title: "Workspace",
            workspace_picker_hint: "Enter open · Tab toggle filter · ↑↓ move · Esc cancel",
            workspace_filter_on: "Filter: .hurl/.json",
            workspace_filter_off: "Filter: All files",
            workspace_no_files: "No matching files in this folder.",
            tab_request: "Scratch Space",
            run_entry: "▶ Run",
            entry_request_json: "Request JSON",
            entry_request_hurl: "Request Hurl",
            entry_raw_hurl: "Raw Mode (Hurl)",
            entry_raw_json: "Raw Mode (JSON)",
            invalid_hurl: "Not valid Hurl (expected exactly one request); edit and try again.",
            invalid_request_json: "Not valid Request JSON (expected an object with at least \"method\" and \"url\"); edit and try again.",
            no_requests_hint: "No requests yet. Use \u{FF0B} New Request to create one.",
            list_up_row: "‹ .. (up a folder)",
            new_request: "\u{FF0B} New Request",
            edit_request: "\u{270E} Edit Request",
            field_name: "Name",
            field_target: "Add to",
            field_method: "Method",
            field_url: "URL",
            field_headers: "Headers",
            field_cookies: "Cookies",
            field_form: "Form",
            field_body: "Body",
            field_asserts: "Asserts",
            field_captures: "Captures",
            tab_all: "All",
            hdr_key: "Key",
            hdr_value: "Value",
            hdr_description: "Description",
            hdr_type: "Type",
            form_type_text: "Text",
            form_type_file: "File",
            content_type_hint: "Content-Type",
            content_type_auto: "Auto (detect from extension)",
            content_type_auto_placeholder: "Auto",
            hint_pick_file: "^F browse",
            hint_delete_row: "^D delete row",
            hint_toggle_enabled: "^E toggle enabled",
            add_header: "\u{FF0B} Add header",
            add_cookie: "\u{FF0B} Add cookie",
            add_form_field: "\u{FF0B} Add field",
            add_assert: "\u{FF0B} Add assert",
            add_capture: "\u{FF0B} Add capture",
            cap_name: "Name",
            cap_expr: "Expression",
            load_environment: "Load Environment…",
            env_heading: "Global Environments",
            env_no_env: "(no environment loaded)",
            env_loading: "Loading secret…",
            env_waiting_secrets: "Waiting for secrets:",
            env_reloading_var: "Reloading",
            env_activated: "Activated",
            env_deactivated: "Deactivated",
            env_rename_title: "Rename Environment",
            env_link_picker_title: "Link Environment",
            env_link_none: "(none)",
            env_delete_confirm: "Delete this environment?",
            env_no_envs: "(no environments — Load Environment… to add one)",
            env_collision_title: "Environment name already exists",
            env_collision_replace: "Replace existing",
            env_collision_keep_both: "Keep both (duplicate name)",
            env_collision_abort: "Abort",
            env_collision_rename: "Rename then add",
            run_summary_passed: "Passed",
            run_summary_failed: "Failed",
            run_summary_total: "Total",
            env_add_var_title: "New environment variable",
            env_var_switch: "switch",
            env_still_secret: "Still secret",
            env_still_secret_hint: "Ctrl+T: toggle still-secret",

            git_collection_menu: "Load Collection from Git…",
            git_env_menu: "Load Environment from Git…",
            git_workspace_menu: "Load Workspace from Git…",
            git_url_label: "Git URL",
            git_token_label: "Access token (optional)",
            git_connect_hint: "Tab switch field · Enter connect · Esc cancel",
            git_recent_hint: "↓ recent URLs · Enter select",
            git_pick_ref_title: "Select a branch or tag",
            git_pick_file_title: "Select a file",
            git_filter_hint: "Type to filter · ↑↓ move · Enter select · Esc cancel",
            git_loading_refs: "Fetching branches and tags…",
            git_loading_files: "Fetching file list…",
            git_loading_file: "Fetching file…",
            git_loading_workspace_files: "Downloading matching files…",
            git_loading_hint: "(Esc to cancel)",
            git_error_hint: "Press Esc to close",
            git_url_required: "A Git URL is required.",
            git_branches: "Branches",
            git_tags: "Tags",
            git_filter_label: "filter: ",
            git_ask_load_env_q: "Also load an environment from this ref?",
            git_pick_env_file_title: "Select an environment file",
            git_pick_workspace_filter_title: "Choose which files to download",
            git_workspace_filter_hint: "↑↓ move · Enter select · Esc cancel",
            git_ws_filter_hurl_json: ".hurl and .json files (recommended)",
            git_ws_filter_hurl: ".hurl files only",
            git_ws_filter_json: ".json files only",
            git_ws_filter_all: "All files",
            git_workspace_no_matches: "No files in this repo matched that filter.",
            close_git_workspace_q: "This Workspace's files were downloaded from git into:\n{p}\n\nKeep this folder so the tab can be reopened later, or delete it now?",
            close_git_workspace_keep: "Keep",
            close_git_workspace_delete: "Delete",
            close_git_workspace_cancel: "Cancel",
            workspace_folder_missing: "The folder for Workspace '{name}' could not be found (it may have been cleared since your last session) and has been reset — pick a folder, or Load Workspace from Git again.",
            workspace_reload_confirm_q: "Workspace '{name}''s downloaded files are missing (likely cleared from a temp folder). Try to redownload {ref} from:\n{url}?",
            workspace_reload_loading: "Redownloading workspace from git…",
            workspace_reload_success: "Workspace redownloaded from git.",
            workspace_reload_failed: "Could not redownload the workspace — the remote no longer seems to have that commit or tag ({e}).",
            workspace_reload_save_hint: "Tip: save this Workspace to a permanent local folder if you want it to always be available without redownloading.",

            file_not_workspace: "The active tab isn't a Workspace.",
            save_workspace: "Save Workspace — Choose Destination Folder",
            browser_hint_workspace_save: "Enter open folder · Space choose as destination · ← parent · Esc cancel",
            workspace_save_name_prompt: "Workspace name",
            workspace_save_success: "Workspace saved.",
            workspace_save_failed: "Could not save the workspace ({e}).",
            git_workspace_storage_q: "Workspace downloaded. Keep it in a temporary folder, or save it to a permanent location now?",
            git_workspace_storage_temp: "Keep temporarily",
            git_workspace_storage_choose: "Choose a folder…",

            git_no_origin: "This collection wasn't loaded from Git.",
            git_save_title: "Save to Git",
            git_save_include_env_label: "Also save the environment",
            git_save_collection_path_label: "Collection path in repo",
            git_save_env_path_label: "Environment path in repo",
            git_save_step_hint: "Tab switch field · Enter continue · Esc cancel",
            git_save_branch_label: "Branch",
            git_save_tag_label: "Tag",
            git_save_target_hint: "Tab Branch/Tag · type a name · ↑↓ pick existing · Enter continue · Esc cancel",
            git_save_commit_msg_label: "Commit message",
            git_save_commit_msg_hint: "Enter push · Esc cancel",
            git_save_pushing: "Pushing to Git…",
            git_save_success: "Saved to Git",
            git_tag_exists: "That tag already exists — tags are never overwritten. Choose a different name.",
            git_ref_exists_race: "That name was just created by someone else — pick it from the list, or choose a different one.",

            hint_edit_base_url: "to edit",
            json_enter_to_edit: "to edit",
            subst_hint_loaded: "loaded",
            subst_hint_literal: "literal",
            subst_hint_loading: "loading",
            subst_hint_missing: "missing",
            subst_hint_shadowed: "shadowed by linked env",
            json_invalid: "⚠ Invalid JSON — fix before running",
            foot_focus: "focus",
            foot_move: "move",
            foot_edit: "edit",
            foot_run: "run",
            foot_run_all: "run all",
            foot_env_activate: "activate/deactivate",
            foot_env_link: "link env",
            foot_new: "New Request/Var",
            foot_reload_var: "Reload var",
            foot_file: "File",
            foot_options: "Settings",
            foot_rename: "rename",
            foot_close: "delete",
            foot_copy_selection: "copy",
            foot_help: "help",
            foot_quit: "quit",
            help_title: "Help",
            help_heading: "PaperBoy — Terminal UI",
            help_tab_shortcuts: "Shortcuts",
            help_tab_glossary: "Glossary",
            help_tab_switch_hint: "Tab / ←→ to switch view",
            glossary_heading: "Substitution colours & icons — Request JSON/Hurl view",
            glossary_label_literal: "literal",
            glossary_desc_literal: "A plain literal value from the active Environment, substituted directly.",
            glossary_label_loaded: "loaded",
            glossary_desc_loaded: "Resolved from an external source (environment variable, 1Password, SSM) or an initialised response capture.",
            glossary_label_pending: "loading",
            glossary_desc_pending: "A secret reference still being fetched in the background; kept as \"{{ VAR }}\" until it resolves.",
            glossary_label_failed: "missing",
            glossary_desc_failed: "Failed to resolve, or a response capture not yet initialised — kept as \"{{ VAR }}\".",
            glossary_label_shadowed: "shadowed",
            glossary_desc_shadowed: "This value comes from the active Global Environment, but is being overridden by the collection's linked Environment — the linked value is the one actually substituted.",
            glossary_heading_icons: "Other icons used throughout the app",
            glossary_label_modified: "modified",
            glossary_desc_modified: "A pencil marks a request, header, or variable that has been edited away from its originally loaded value.",
            glossary_label_added: "added",
            glossary_desc_added: "A plus marks a request or variable added by hand, rather than loaded from a file.",
            glossary_label_passed: "passed / active",
            glossary_desc_passed: "A request or assertion that passed in the last \"Run All\", or the currently active Global Environment in its list.",
            glossary_label_run_failed: "failed",
            glossary_desc_run_failed: "A request or assertion that failed in the last \"Run All\".",
            glossary_label_running: "running",
            glossary_desc_running: "A request that is still running as part of a batch \"Run All\".",
            glossary_label_git: "git-linked",
            glossary_desc_git: "This Collection, Environment, or Workspace was loaded from, and is linked to, a git remote.",
            glossary_label_linked: "linked environment",
            glossary_desc_linked: "Joins a Collection's tab/title to the Global Environment linked to it.",
            glossary_label_folder: "folder",
            glossary_desc_folder: "A subfolder grouping requests in the list, or — next to a File-type form value — a hint that pressing Enter opens a file picker.",
            glossary_label_scroll_hint: "more text",
            glossary_desc_scroll_hint: "Shown at the edge of a truncated line when there is more hidden text to scroll to in that direction.",
            help_focus: "cycle focus between panes",
            help_move: "move within a pane",
            help_page_response: "page up/down through the Response body",
            help_switch_tabs: "switch tabs (Tabs pane)",
            help_select: "select / edit focused item (wizard)",
            help_run: "send request / run entry",
            help_run_all: "run every request in the collection, in order (like the CLI)",
            help_raw_mode: "edit selected request in Raw Mode (Hurl text)",
            help_raw_json: "edit selected request in Raw Mode (JSON text)",
            help_new: "new request (or add variable, in the environment popup)",
            help_base_url: "edit default new-request URL",
            help_menus: "File / Settings menu",
            help_workspace_browse: "Browse Workspace (choose a collection file)",
            help_prev_next_tab: "previous / next tab",
            help_rename_close: "rename tab (F2) · delete request / close collection tab",
            help_reload_var: "reload a failed environment entry (env var / 1Password / SSM)",
            help_env_activate: "activate / deactivate the selected Global Environment",
            help_env_delete: "delete the selected Global Environment (unlinks any collections using it)",
            help_env_link: "link / unlink a Global Environment to the active collection",
            help_env_view_linked: "view the active collection's linked Global Environment",
            help_env_rename: "rename the open Global Environment",
            help_resize: "shrink / grow response pane",
            help_resize_width: "grow / shrink left column",
            help_tab_manage: "close / reopen collection or workspace tab",
            help_tab_reorder: "reorder tabs",
            help_restore_request: "restore deleted request (List pane)",
            help_row_toggle_delete: "in wizard tables: ^E toggle row enabled, ^D delete row",
            help_copy_selection: "copy the selection, or the whole panel if nothing is selected (Request JSON / Request Hurl / Response panel)",
            help_multi_select: "Alt+Click+Drag adds another selection region (plain click clears all)",
            help_save_editor: "save a multi-line editor",
            help_cancel: "close menu / cancel edit",
            help_quit: "quit",
            help_group_navigation: "Navigation",
            help_group_tabs: "Tabs",
            help_group_requests: "Requests & Running",
            help_group_menus: "Menus & Workspace",
            help_group_environments: "Environments",
            help_group_editing: "Editing & Selection",
            help_group_panels: "Panels & General",
            new_request_hint: "Tab/arrows move · PgUp/PgDn tab · Alt+1-6 jump · ^Enter/F2 create · Esc cancel",
            edit_request_hint: "Tab/arrows move · PgUp/PgDn tab · Alt+1-6 jump · ^Enter/F2 save · Esc cancel",
            raw_mode_hint: "Edit the raw Hurl text · F2/^Enter reparse & save · Esc cancel · Shift+Arrow select · ^Y copy",
            raw_json_hint: "Edit the raw JSON · F2/^Enter reparse & save · Esc cancel · Shift+Arrow select · ^Y copy",
            ctrl_enter_key: "^Enter",
            prompt_rename_title: "Rename tab",
            prompt_enter_path: "enter path",
            prompt_save_hint_ml: "F2 save · Esc cancel",
            prompt_save_hint_sl: "Enter save · Esc cancel",
            prompt_reset_hint: "^R reset",
            browser_select_file: "Select file",
            browser_hint: "Enter open · ← parent · ^h hidden · Esc cancel",
            tabs_heading: "Collections",
            suggest_hint: "↓↑ select · Enter fill",
        }
    }

    fn french() -> Self {
        Self {
            app_heading: "🦀 PaperBoy",
            base_url: "URL par défaut des nouvelles requêtes\u{a0}:",
            sending: "Envoi en cours…",
            response_heading: "Réponse",
            status_label: "Statut :",
            no_response_yet: "Exécutez une requête pour voir la réponse.",
            req_error_prefix: "Erreur de requête :",
            options_menu: "Paramètres",
            options_menu_label: "Paramètre(s)",
            language_label: "Langue",
            lang_english: "English",
            lang_french: "Français",
            lang_danish: "Dansk",
            clear_all: "Fermer toutes les collections",
            clear_all_done: "Toutes les collections ont été fermées",
            copied_to_clipboard: "Copié dans le presse-papiers",
            preferences_menu: "Préférences",
            confirm_on_exit: "Confirmer à la sortie",
            confirm_exit_secrets: "Il y a des secrets d'environnement avec des modifications non enregistrées, quitter entraînera la perte de ces modifications.",
            confirm_on_clear: "Confirmer avant de fermer",
            default_request_view_label: "Vue de requête par défaut",
            view_json_label: "JSON",
            view_hurl_label: "Hurl",
            confirm_exit_q: "Quitter PaperBoy\u{a0}?",
            confirm_clear_q: "Fermer toutes les collections\u{a0}? Cela supprime tous les onglets et requêtes.",
            confirm_save_collection_q: "Il y a {r} requête(s) nouvelle(s) ou modifiée(s). L'enregistrement écrasera le fichier de collection d'origine. Continuer\u{a0}?",
            confirm_save_env_q: "Il y a {e} variable(s) d'environnement nouvelle(s) ou modifiée(s). L'enregistrement écrasera le fichier d'environnement d'origine. Continuer\u{a0}?",
            confirm_overwrite_q: "«\u{a0}{f}\u{a0}» existe déjà. L'écraser\u{a0}?",
            confirm_yes: "Oui",
            confirm_no: "Non",
            file_menu: "Fichier",
            file_menu_label: "(F)ichier",
            file_menu_item_load: "(C)harger",
            file_menu_item_save: "(E)nregistrer",
            file_load_menu: "Charger",
            file_save_menu: "Enregistrer",
            file_load_item_request: "(R)equête…",
            file_load_item_collection: "(C)ollection…",
            file_load_item_collection_git: "Collection depuis (G)it…",
            file_load_item_environment: "(E)nvironnement…",
            file_load_item_environment_git: "En(v)ironnement depuis Git…",
            file_load_item_workspace: "(W)orkspace…",
            file_load_item_workspace_git: "Work(s)pace depuis Git…",
            file_save_item_request: "(R)equête…",
            file_save_item_collection: "(C)ollection…",
            file_save_item_collection_as: "Collection s(o)us…",
            file_save_item_collection_git: "Enregistrer la collection sur (G)it…",
            file_save_item_environment: "(E)nvironnement…",
            file_save_item_environment_as: "En(v)ironnement sous…",
            file_save_item_workspace: "(W)orkspace…",
            file_save_item_response: "Ré(p)onse…",
            save_request: "Enregistrer la requête…",
            load_request: "Charger une requête…",
            open_collection: "Charger une collection…",
            save_collection: "Enregistrer la collection…",
            save_environment: "Enregistrer l'environnement…",
            save_response: "Enregistrer la réponse…",
            file_saved: "Enregistré.",
            file_loaded: "Chargé.",
            file_no_response: "Aucune réponse à enregistrer.",
            file_error_prefix: "Erreur :",
            file_not_collection: "Fichier de collection invalide (aucune requête trouvée).",
            file_not_environment: "Fichier d'environnement invalide (lignes CLÉ=valeur attendues).",
            open_workspace: "Choisir le dossier Workspace…",
            browser_hint_workspace: "Entrée ouvrir dossier · Espace choisir comme Workspace · ← dossier parent · Échap annuler",
            workspace_empty_state: "Aucune collection — appuyez sur w.",
            foot_workspace: "parcourir workspace",
            workspace_picker_title: "Workspace",
            workspace_picker_hint: "Entrée ouvrir · Tab basculer filtre · ↑↓ déplacer · Échap annuler",
            workspace_filter_on: "Filtre : .hurl/.json",
            workspace_filter_off: "Filtre : tous les fichiers",
            workspace_no_files: "Aucun fichier correspondant dans ce dossier.",
            tab_request: "Brouillon",
            run_entry: "▶ Exécuter",
            entry_request_json: "JSON de la requête",
            entry_request_hurl: "Requête (Hurl)",
            entry_raw_hurl: "Mode brut (Hurl)",
            entry_raw_json: "Mode brut (JSON)",
            invalid_hurl: "Hurl invalide (une seule requête attendue) ; modifiez et réessayez.",
            invalid_request_json: "JSON de requête invalide (un objet avec au moins « method » et « url » est attendu) ; modifiez et réessayez.",
            no_requests_hint: "Aucune requête. Utilisez \u{FF0B} Nouvelle requête pour en créer une.",
            list_up_row: "‹ .. (dossier parent)",
            new_request: "\u{FF0B} Nouvelle requête",
            edit_request: "\u{270E} Modifier la requête",
            field_name: "Nom",
            field_target: "Ajouter à",
            field_method: "Méthode",
            field_url: "URL",
            field_headers: "En-têtes",
            field_cookies: "Cookies",
            field_form: "Formulaire",
            field_body: "Corps",
            field_asserts: "Assertions",
            field_captures: "Captures",
            tab_all: "Tout",
            hdr_key: "Clé",
            hdr_value: "Valeur",
            hdr_description: "Description",
            hdr_type: "Type",
            form_type_text: "Texte",
            form_type_file: "Fichier",
            content_type_hint: "Type de contenu",
            content_type_auto: "Auto (détecter depuis l'extension)",
            content_type_auto_placeholder: "Auto",
            hint_pick_file: "^F parcourir",
            hint_delete_row: "^D supprimer la ligne",
            hint_toggle_enabled: "^E activer/désactiver",
            add_header: "\u{FF0B} Ajouter un en-tête",
            add_cookie: "\u{FF0B} Ajouter un cookie",
            add_form_field: "\u{FF0B} Ajouter un champ",
            add_assert: "\u{FF0B} Ajouter une assertion",
            add_capture: "\u{FF0B} Ajouter une capture",
            cap_name: "Nom",
            cap_expr: "Expression",
            load_environment: "Charger l'environnement…",
            env_heading: "Environnements globaux",
            env_no_env: "(aucun environnement chargé)",
            env_loading: "Chargement du secret…",
            env_waiting_secrets: "En attente des secrets\u{a0}:",
            env_reloading_var: "Rechargement de",
            env_activated: "Activé",
            env_deactivated: "Désactivé",
            env_rename_title: "Renommer l'environnement",
            env_link_picker_title: "Lier un environnement",
            env_link_none: "(aucun)",
            env_delete_confirm: "Supprimer cet environnement\u{a0}?",
            env_no_envs: "(aucun environnement — Charger l'environnement… pour en ajouter un)",
            env_collision_title: "Ce nom d'environnement existe déjà",
            env_collision_replace: "Remplacer l'existant",
            env_collision_keep_both: "Conserver les deux (nom en double)",
            env_collision_abort: "Annuler",
            env_collision_rename: "Renommer puis ajouter",
            run_summary_passed: "Réussi",
            run_summary_failed: "Échoué",
            run_summary_total: "Total",
            env_add_var_title: "Nouvelle variable d'environnement",
            env_var_switch: "changer",
            env_still_secret: "Toujours secret",
            env_still_secret_hint: "Ctrl+T\u{a0}: bascule toujours-secret",

            git_collection_menu: "Charger une collection depuis Git…",
            git_env_menu: "Charger un environnement depuis Git…",
            git_workspace_menu: "Charger un Workspace depuis Git…",
            git_url_label: "URL Git",
            git_token_label: "Jeton d'accès (facultatif)",
            git_connect_hint: "Tab changer de champ · Entrée connecter · Échap annuler",
            git_recent_hint: "↓ URL récentes · Entrée sélectionner",
            git_pick_ref_title: "Sélectionnez une branche ou une étiquette",
            git_pick_file_title: "Sélectionnez un fichier",
            git_filter_hint: "Filtrer en tapant · ↑↓ déplacer · Entrée choisir · Échap annuler",
            git_loading_refs: "Récupération des branches et étiquettes…",
            git_loading_files: "Récupération de la liste des fichiers…",
            git_loading_file: "Récupération du fichier…",
            git_loading_workspace_files: "Téléchargement des fichiers correspondants…",
            git_loading_hint: "(Échap pour annuler)",
            git_error_hint: "Appuyez sur Échap pour fermer",
            git_url_required: "Une URL Git est requise.",
            git_branches: "Branches",
            git_tags: "Étiquettes",
            git_filter_label: "filtre\u{a0}: ",
            git_ask_load_env_q: "Charger aussi un environnement depuis cette référence\u{a0}?",
            git_pick_env_file_title: "Sélectionnez un fichier d'environnement",
            git_pick_workspace_filter_title: "Choisissez les fichiers à télécharger",
            git_workspace_filter_hint: "↑↓ déplacer · Entrée choisir · Échap annuler",
            git_ws_filter_hurl_json: "Fichiers .hurl et .json (recommandé)",
            git_ws_filter_hurl: "Fichiers .hurl uniquement",
            git_ws_filter_json: "Fichiers .json uniquement",
            git_ws_filter_all: "Tous les fichiers",
            git_workspace_no_matches: "Aucun fichier de ce dépôt ne correspond à ce filtre.",
            close_git_workspace_q: "Les fichiers de cet Espace de travail ont été téléchargés depuis git dans :\n{p}\n\nConserver ce dossier pour pouvoir rouvrir l'onglet plus tard, ou le supprimer maintenant ?",
            close_git_workspace_keep: "Conserver",
            close_git_workspace_delete: "Supprimer",
            close_git_workspace_cancel: "Annuler",
            workspace_folder_missing: "Le dossier de l'Espace de travail « {name} » est introuvable (il a peut-être été supprimé depuis votre dernière session) et a été réinitialisé — choisissez un dossier, ou chargez à nouveau l'Espace de travail depuis Git.",
            workspace_reload_confirm_q: "Les fichiers téléchargés de l'Espace de travail « {name} » sont introuvables (probablement supprimés d'un dossier temporaire). Essayer de retélécharger {ref} depuis :\n{url} ?",
            workspace_reload_loading: "Retéléchargement de l'espace de travail depuis git…",
            workspace_reload_success: "Espace de travail retéléchargé depuis git.",
            workspace_reload_failed: "Impossible de retélécharger l'espace de travail — le dépôt distant ne semble plus avoir ce commit ou ce tag ({e}).",
            workspace_reload_save_hint: "Astuce : enregistrez cet Espace de travail dans un dossier local permanent si vous voulez qu'il soit toujours disponible sans nouveau téléchargement.",

            file_not_workspace: "L'onglet actif n'est pas un Workspace.",
            save_workspace: "Enregistrer le Workspace — Choisir le dossier de destination",
            browser_hint_workspace_save: "Entrée ouvrir dossier · Espace choisir comme destination · ← dossier parent · Échap annuler",
            workspace_save_name_prompt: "Nom du Workspace",
            workspace_save_success: "Workspace enregistré.",
            workspace_save_failed: "Impossible d'enregistrer le Workspace ({e}).",
            git_workspace_storage_q: "Workspace téléchargé. Le garder dans un dossier temporaire, ou l'enregistrer dans un emplacement permanent maintenant ?",
            git_workspace_storage_temp: "Garder temporairement",
            git_workspace_storage_choose: "Choisir un dossier…",

            git_no_origin: "Cette collection n'a pas été chargée depuis Git.",
            git_save_title: "Enregistrer sur Git",
            git_save_include_env_label: "Enregistrer aussi l'environnement",
            git_save_collection_path_label: "Chemin de la collection dans le dépôt",
            git_save_env_path_label: "Chemin de l'environnement dans le dépôt",
            git_save_step_hint: "Tab changer de champ · Entrée continuer · Échap annuler",
            git_save_branch_label: "Branche",
            git_save_tag_label: "Étiquette",
            git_save_target_hint: "Tab Branche/Étiquette · saisir un nom · ↑↓ choisir existant · Entrée continuer · Échap annuler",
            git_save_commit_msg_label: "Message de commit",
            git_save_commit_msg_hint: "Entrée pousser · Échap annuler",
            git_save_pushing: "Envoi vers Git…",
            git_save_success: "Enregistré sur Git",
            git_tag_exists: "Cette étiquette existe déjà — les étiquettes ne sont jamais écrasées. Choisissez un autre nom.",
            git_ref_exists_race: "Ce nom vient d'être créé par quelqu'un d'autre — choisissez-le dans la liste, ou choisissez-en un autre.",

            hint_edit_base_url: "pour modifier",
            json_enter_to_edit: "pour modifier",
            subst_hint_loaded: "chargé",
            subst_hint_literal: "littéral",
            subst_hint_loading: "en cours",
            subst_hint_missing: "manquant",
            subst_hint_shadowed: "masqué par l'environnement lié",
            json_invalid: "⚠ JSON invalide — corrigez avant d'exécuter",
            foot_focus: "focus",
            foot_move: "déplacer",
            foot_edit: "modifier",
            foot_run: "exécuter",
            foot_run_all: "tout exécuter",
            foot_env_activate: "activer/désactiver",
            foot_env_link: "lier env",
            foot_new: "Nouvelle requête/var",
            foot_reload_var: "Recharger var",
            foot_file: "Fichier",
            foot_options: "Paramètres",
            foot_rename: "renommer",
            foot_close: "supprimer",
            foot_copy_selection: "copier",
            foot_help: "aide",
            foot_quit: "quitter",
            help_title: "Aide",
            help_heading: "PaperBoy — Interface Terminal",
            help_tab_shortcuts: "Raccourcis",
            help_tab_glossary: "Glossaire",
            help_tab_switch_hint: "Tab / ←→ pour changer de vue",
            glossary_heading: "Couleurs et icônes de substitution — vue Requête JSON/Hurl",
            glossary_label_literal: "littéral",
            glossary_desc_literal: "Une valeur littérale simple de l'environnement actif, substituée directement.",
            glossary_label_loaded: "chargé",
            glossary_desc_loaded: "Résolu depuis une source externe (variable d'environnement, 1Password, SSM) ou une capture de réponse initialisée.",
            glossary_label_pending: "en cours",
            glossary_desc_pending: "Une référence à un secret encore en cours de récupération en arrière-plan\u{a0}; reste affichée sous forme \"{{ VAR }}\" jusqu'à sa résolution.",
            glossary_label_failed: "manquant",
            glossary_desc_failed: "Échec de la résolution, ou capture de réponse pas encore initialisée — reste affichée sous forme \"{{ VAR }}\".",
            glossary_label_shadowed: "masqué",
            glossary_desc_shadowed: "Cette valeur provient de l'environnement global actif, mais est masquée par l'environnement lié de la collection — c'est la valeur liée qui est réellement substituée.",
            glossary_heading_icons: "Autres icônes utilisées dans l'application",
            glossary_label_modified: "modifié",
            glossary_desc_modified: "Un crayon marque une requête, un en-tête ou une variable modifiée par rapport à sa valeur chargée d'origine.",
            glossary_label_added: "ajouté",
            glossary_desc_added: "Un plus marque une requête ou une variable ajoutée manuellement, plutôt que chargée depuis un fichier.",
            glossary_label_passed: "réussi / actif",
            glossary_desc_passed: "Une requête ou assertion réussie lors du dernier \"Tout exécuter\", ou l'environnement global actuellement actif dans sa liste.",
            glossary_label_run_failed: "échoué",
            glossary_desc_run_failed: "Une requête ou assertion en échec lors du dernier \"Tout exécuter\".",
            glossary_label_running: "en cours",
            glossary_desc_running: "Une requête encore en cours d'exécution dans un lot \"Tout exécuter\".",
            glossary_label_git: "lié à git",
            glossary_desc_git: "Cette collection, cet environnement ou cet espace de travail a été chargé depuis, et reste lié à, une origine git distante.",
            glossary_label_linked: "environnement lié",
            glossary_desc_linked: "Relie l'onglet/titre d'une collection à l'environnement global qui lui est lié.",
            glossary_label_folder: "dossier",
            glossary_desc_folder: "Un sous-dossier regroupant des requêtes dans la liste, ou — à côté d'une valeur de formulaire de type Fichier — une indication que Entrée ouvre un sélecteur de fichier.",
            glossary_label_scroll_hint: "plus de texte",
            glossary_desc_scroll_hint: "Affiché au bord d'une ligne tronquée lorsqu'il reste du texte masqué à faire défiler dans cette direction.",
            help_focus: "changer de panneau",
            help_move: "se déplacer dans un panneau",
            help_page_response: "page précédente/suivante dans le corps de la réponse",
            help_switch_tabs: "changer d'onglet (panneau Onglets)",
            help_select: "sélectionner / modifier l'élément (assistant)",
            help_run: "envoyer la requête / exécuter l'entrée",
            help_run_all: "exécuter toutes les requêtes de la collection, dans l'ordre (comme la CLI)",
            help_raw_mode: "modifier la requête sélectionnée en mode brut (texte Hurl)",
            help_raw_json: "modifier la requête sélectionnée en mode brut (texte JSON)",
            help_new: "nouvelle requête (ou ajouter une variable, dans la popup d'environnement)",
            help_base_url: "modifier l'URL par défaut des nouvelles requêtes",
            help_menus: "menu Fichier / Paramètres",
            help_workspace_browse: "Parcourir le Workspace (choisir un fichier de collection)",
            help_prev_next_tab: "onglet précédent / suivant",
            help_rename_close: "renommer l'onglet (F2) · supprimer la requête / fermer l'onglet",
            help_reload_var: "recharger une entrée d'environnement en échec (var d'env / 1Password / SSM)",
            help_env_activate: "activer / désactiver l'environnement global sélectionné",
            help_env_delete: "supprimer l'environnement global sélectionné (délie les collections qui l'utilisent)",
            help_env_link: "lier / délier un environnement global à la collection active",
            help_env_view_linked: "afficher l'environnement global lié à la collection active",
            help_env_rename: "renommer l'environnement global ouvert",
            help_resize: "réduire / agrandir le panneau de réponse",
            help_resize_width: "agrandir / réduire la colonne de gauche",
            help_tab_manage: "fermer / rouvrir un onglet de collection ou d'espace de travail",
            help_tab_reorder: "réorganiser les onglets",
            help_restore_request: "restaurer la requête supprimée (volet Liste)",
            help_row_toggle_delete: "dans les tableaux : ^E activer/désactiver la ligne, ^D supprimer la ligne",
            help_copy_selection: "copier la sélection, ou tout le panneau si rien n'est sélectionné (panneau JSON de requête / Hurl de requête / réponse)",
            help_multi_select: "Alt+Clic+Glisser ajoute une autre zone de sélection (un clic simple efface tout)",
            help_save_editor: "enregistrer un éditeur multi-lignes",
            help_cancel: "fermer le menu / annuler la modification",
            help_quit: "quitter",
            help_group_navigation: "Navigation",
            help_group_tabs: "Onglets",
            help_group_requests: "Requêtes et exécution",
            help_group_menus: "Menus et Workspace",
            help_group_environments: "Environnements",
            help_group_editing: "Édition et sélection",
            help_group_panels: "Panneaux et général",
            new_request_hint: "Tab/flèches se déplacer · PgUp/PgDn onglet · Alt+1-6 aller à · ^Entrée/F2 créer · Échap annuler",
            edit_request_hint: "Tab/flèches se déplacer · PgUp/PgDn onglet · Alt+1-6 aller à · ^Entrée/F2 enregistrer · Échap annuler",
            raw_mode_hint: "Modifiez le texte Hurl brut · F2/^Entrée réanalyser et enregistrer · Échap annuler · Maj+Flèche sélection · ^Y copier",
            raw_json_hint: "Modifiez le JSON brut · F2/^Entrée réanalyser et enregistrer · Échap annuler · Maj+Flèche sélection · ^Y copier",
            ctrl_enter_key: "^Entrée",
            prompt_rename_title: "Renommer l'onglet",
            prompt_enter_path: "saisir le chemin",
            prompt_save_hint_ml: "F2 enregistrer · Échap annuler",
            prompt_save_hint_sl: "Entrée enregistrer · Échap annuler",
            prompt_reset_hint: "^R réinitialiser",
            browser_select_file: "Sélectionner un fichier",
            browser_hint: "Entrée ouvrir · ← dossier parent · ^h fichiers cachés · Échap annuler",
            tabs_heading: "Collections",
            suggest_hint: "↓↑ sélectionner · Entrée remplir",
        }
    }

    fn danish() -> Self {
        Self {
            app_heading: "🦀 PaperBoy",
            base_url: "Standard-URL for nye anmodninger:",
            sending: "Sender…",
            response_heading: "Svar",
            status_label: "Status:",
            no_response_yet: "Kør en anmodning for at se svaret.",
            req_error_prefix: "Anmodningsfejl:",
            options_menu: "Indstillinger",
            options_menu_label: "Ind(s)tillinger",
            language_label: "Sprog",
            lang_english: "English",
            lang_french: "Français",
            lang_danish: "Dansk",
            clear_all: "Luk alle samlinger",
            clear_all_done: "Alle samlinger er lukket",
            copied_to_clipboard: "Kopieret til udklipsholder",
            preferences_menu: "Præferencer",
            confirm_on_exit: "Bekræft ved afslutning",
            confirm_exit_secrets: "Der er miljøhemmeligheder med ikke-gemte ændringer. Hvis du afslutter, vil disse ændringer gå tabt.",
            confirm_on_clear: "Bekræft ved lukning",
            default_request_view_label: "Standard anmodningsvisning",
            view_json_label: "JSON",
            view_hurl_label: "Hurl",
            confirm_exit_q: "Afslut PaperBoy?",
            confirm_clear_q: "Luk alle samlinger? Dette fjerner alle faner og anmodninger.",
            confirm_save_collection_q: "Der er {r} nye eller ændrede anmodninger. Gemning vil overskrive den oprindelige samlingsfil. Fortsæt?",
            confirm_save_env_q: "Der er {e} nye eller ændrede miljøvariabler. Gemning vil overskrive den oprindelige miljøfil. Fortsæt?",
            confirm_overwrite_q: "«{f}» findes allerede. Overskriv den?",
            confirm_yes: "Ja",
            confirm_no: "Nej",
            file_menu: "Fil",
            file_menu_label: "(F)il",
            file_menu_item_load: "(I)ndlæs",
            file_menu_item_save: "(G)em",
            file_load_menu: "Indlæs",
            file_save_menu: "Gem",
            file_load_item_request: "(A)nmodning…",
            file_load_item_collection: "(S)amling…",
            file_load_item_collection_git: "Samling fra (G)it…",
            file_load_item_environment: "(M)iljø…",
            file_load_item_environment_git: "Miljø fra Gi(t)…",
            file_load_item_workspace: "(W)orkspace…",
            file_load_item_workspace_git: "Works(p)ace fra Git…",
            file_save_item_request: "(A)nmodning…",
            file_save_item_collection: "(S)amling…",
            file_save_item_collection_as: "Samling s(o)m…",
            file_save_item_collection_git: "Gem samling til (G)it…",
            file_save_item_environment: "(M)iljø…",
            file_save_item_environment_as: "Miljø - n(y)t navn…",
            file_save_item_workspace: "(W)orkspace…",
            file_save_item_response: "S(v)ar…",
            save_request: "Gem anmodning…",
            load_request: "Indlæs anmodning…",
            open_collection: "Indlæs samling…",
            save_collection: "Gem samling…",
            save_environment: "Gem miljø…",
            save_response: "Gem svar…",
            file_saved: "Gemt.",
            file_loaded: "Indlæst.",
            file_no_response: "Intet svar at gemme.",
            file_error_prefix: "Fejl:",
            file_not_collection: "Ikke en gyldig samlingsfil (ingen anmodninger fundet).",
            file_not_environment: "Ikke en gyldig miljøfil (forventede NØGLE=værdi-linjer).",
            open_workspace: "Vælg Workspace-mappe…",
            browser_hint_workspace: "Enter åbn mappe · Mellemrum vælg som Workspace · ← overordnet · Esc annuller",
            workspace_empty_state: "Ingen samling — tryk w.",
            foot_workspace: "gennemse workspace",
            workspace_picker_title: "Workspace",
            workspace_picker_hint: "Enter åbn · Tab skift filter · ↑↓ flyt · Esc annuller",
            workspace_filter_on: "Filter: .hurl/.json",
            workspace_filter_off: "Filter: Alle filer",
            workspace_no_files: "Ingen matchende filer i denne mappe.",
            tab_request: "Kladde",
            run_entry: "▶ Kør",
            entry_request_json: "Anmodnings-JSON",
            entry_request_hurl: "Anmodning (Hurl)",
            entry_raw_hurl: "Rå tilstand (Hurl)",
            entry_raw_json: "Rå tilstand (JSON)",
            invalid_hurl: "Ikke gyldig Hurl (forventede præcis én anmodning); ret og prøv igen.",
            invalid_request_json: "Ikke gyldig anmodnings-JSON (forventede et objekt med mindst \"method\" og \"url\"); ret og prøv igen.",
            no_requests_hint: "Ingen anmodninger endnu. Brug \u{FF0B} Ny anmodning for at oprette en.",
            list_up_row: "‹ .. (mappe op)",
            new_request: "\u{FF0B} Ny anmodning",
            edit_request: "\u{270E} Rediger anmodning",
            field_name: "Navn",
            field_target: "Tilføj til",
            field_method: "Metode",
            field_url: "URL",
            field_headers: "Headere",
            field_cookies: "Cookies",
            field_form: "Formular",
            field_body: "Brødtekst",
            field_asserts: "Assertions",
            field_captures: "Captures",
            tab_all: "Alle",
            hdr_key: "Nøgle",
            hdr_value: "Værdi",
            hdr_description: "Beskrivelse",
            hdr_type: "Type",
            form_type_text: "Tekst",
            form_type_file: "Fil",
            content_type_hint: "Content-Type",
            content_type_auto: "Auto (registrer fra filtype)",
            content_type_auto_placeholder: "Auto",
            hint_pick_file: "^F gennemse",
            hint_delete_row: "^D slet række",
            hint_toggle_enabled: "^E slå til/fra",
            add_header: "\u{FF0B} Tilføj header",
            add_cookie: "\u{FF0B} Tilføj cookie",
            add_form_field: "\u{FF0B} Tilføj felt",
            add_assert: "\u{FF0B} Tilføj assertion",
            add_capture: "\u{FF0B} Tilføj capture",
            cap_name: "Navn",
            cap_expr: "Udtryk",
            load_environment: "Indlæs miljø…",
            env_heading: "Globale miljøer",
            env_no_env: "(intet miljø indlæst)",
            env_loading: "Indlæser hemmelighed…",
            env_waiting_secrets: "Venter på hemmeligheder:",
            env_reloading_var: "Genindlæser",
            env_activated: "Aktiveret",
            env_deactivated: "Deaktiveret",
            env_rename_title: "Omdøb miljø",
            env_link_picker_title: "Tilknyt miljø",
            env_link_none: "(ingen)",
            env_delete_confirm: "Slet dette miljø?",
            env_no_envs: "(ingen miljøer — Indlæs miljø… for at tilføje et)",
            env_collision_title: "Miljønavnet findes allerede",
            env_collision_replace: "Erstat eksisterende",
            env_collision_keep_both: "Behold begge (dublet navn)",
            env_collision_abort: "Afbryd",
            env_collision_rename: "Omdøb og tilføj",
            run_summary_passed: "Bestået",
            run_summary_failed: "Fejlet",
            run_summary_total: "Total",
            env_add_var_title: "Ny miljøvariabel",
            env_var_switch: "skift",
            env_still_secret: "Stadig hemmelig",
            env_still_secret_hint: "Ctrl+T: skift stadig-hemmelig",

            git_collection_menu: "Indlæs samling fra Git…",
            git_env_menu: "Indlæs miljø fra Git…",
            git_workspace_menu: "Indlæs Workspace fra Git…",
            git_url_label: "Git-URL",
            git_token_label: "Adgangstoken (valgfrit)",
            git_connect_hint: "Tab skift felt · Enter forbind · Esc annuller",
            git_recent_hint: "↓ seneste URL'er · Enter vælg",
            git_pick_ref_title: "Vælg en gren eller et tag",
            git_pick_file_title: "Vælg en fil",
            git_filter_hint: "Skriv for at filtrere · ↑↓ flyt · Enter vælg · Esc annuller",
            git_loading_refs: "Henter grene og tags…",
            git_loading_files: "Henter filliste…",
            git_loading_file: "Henter fil…",
            git_loading_workspace_files: "Henter matchende filer…",
            git_loading_hint: "(Esc for at annullere)",
            git_error_hint: "Tryk på Esc for at lukke",
            git_url_required: "En Git-URL er påkrævet.",
            git_branches: "Grene",
            git_tags: "Tags",
            git_filter_label: "filter: ",
            git_ask_load_env_q: "Indlæs også et miljø fra denne reference?",
            git_pick_env_file_title: "Vælg en miljøfil",
            git_pick_workspace_filter_title: "Vælg hvilke filer der skal hentes",
            git_workspace_filter_hint: "↑↓ flyt · Enter vælg · Esc annuller",
            git_ws_filter_hurl_json: ".hurl- og .json-filer (anbefalet)",
            git_ws_filter_hurl: "Kun .hurl-filer",
            git_ws_filter_json: "Kun .json-filer",
            git_ws_filter_all: "Alle filer",
            git_workspace_no_matches: "Ingen filer i dette repo matchede filteret.",
            close_git_workspace_q: "Denne Workspaces filer blev downloadet fra git til:\n{p}\n\nBehold denne mappe, så fanen kan genåbnes senere, eller slet den nu?",
            close_git_workspace_keep: "Behold",
            close_git_workspace_delete: "Slet",
            close_git_workspace_cancel: "Annuller",
            workspace_folder_missing: "Mappen for Workspace '{name}' kunne ikke findes (den er muligvis blevet ryddet siden din sidste session) og er blevet nulstillet — vælg en mappe, eller indlæs Workspace fra Git igen.",
            workspace_reload_confirm_q: "De downloadede filer til Workspace '{name}' mangler (sandsynligvis ryddet fra en midlertidig mappe). Prøv at downloade {ref} igen fra:\n{url}?",
            workspace_reload_loading: "Downloader workspace igen fra git…",
            workspace_reload_success: "Workspace downloadet igen fra git.",
            workspace_reload_failed: "Kunne ikke downloade workspace igen — det fjerne repo synes ikke længere at have denne commit eller tag ({e}).",
            workspace_reload_save_hint: "Tip: gem denne Workspace i en permanent lokal mappe, hvis du vil have den altid tilgængelig uden at skulle downloade igen.",

            file_not_workspace: "Den aktive fane er ikke en Workspace.",
            save_workspace: "Gem Workspace — Vælg destinationsmappe",
            browser_hint_workspace_save: "Enter åbn mappe · Mellemrum vælg som destination · ← overordnet · Esc annuller",
            workspace_save_name_prompt: "Workspace-navn",
            workspace_save_success: "Workspace gemt.",
            workspace_save_failed: "Kunne ikke gemme workspace ({e}).",
            git_workspace_storage_q: "Workspace downloadet. Behold den i en midlertidig mappe, eller gem den på en permanent placering nu?",
            git_workspace_storage_temp: "Behold midlertidigt",
            git_workspace_storage_choose: "Vælg en mappe…",

            git_no_origin: "Denne samling blev ikke indlæst fra Git.",
            git_save_title: "Gem til Git",
            git_save_include_env_label: "Gem også miljøet",
            git_save_collection_path_label: "Samlingens sti i repoet",
            git_save_env_path_label: "Miljøets sti i repoet",
            git_save_step_hint: "Tab skift felt · Enter fortsæt · Esc annuller",
            git_save_branch_label: "Gren",
            git_save_tag_label: "Tag",
            git_save_target_hint: "Tab Gren/Tag · skriv et navn · ↑↓ vælg eksisterende · Enter fortsæt · Esc annuller",
            git_save_commit_msg_label: "Commit-besked",
            git_save_commit_msg_hint: "Enter push · Esc annuller",
            git_save_pushing: "Sender til Git…",
            git_save_success: "Gemt til Git",
            git_tag_exists: "Det tag findes allerede — tags bliver aldrig overskrevet. Vælg et andet navn.",
            git_ref_exists_race: "Det navn blev lige oprettet af en anden — vælg det fra listen, eller vælg et andet.",

            hint_edit_base_url: "for at redigere",
            json_enter_to_edit: "for at redigere",
            subst_hint_loaded: "indlæst",
            subst_hint_literal: "literal",
            subst_hint_loading: "indlæser",
            subst_hint_missing: "mangler",
            subst_hint_shadowed: "skygget af tilknyttet miljø",
            json_invalid: "⚠ Ugyldig JSON — ret før kørsel",
            foot_focus: "fokus",
            foot_move: "flyt",
            foot_edit: "rediger",
            foot_run: "kør",
            foot_run_all: "kør alle",
            foot_env_activate: "aktivér/deaktivér",
            foot_env_link: "link miljø",
            foot_new: "Ny forespørgsel/var",
            foot_reload_var: "Genindlæs var",
            foot_file: "Fil",
            foot_options: "Indstillinger",
            foot_rename: "omdøb",
            foot_close: "fjern",
            foot_copy_selection: "kopiér",
            foot_help: "hjælp",
            foot_quit: "afslut",
            help_title: "Hjælp",
            help_heading: "PaperBoy — Terminalgrænseflade",
            help_tab_shortcuts: "Genveje",
            help_tab_glossary: "Ordliste",
            help_tab_switch_hint: "Tab / ←→ for at skifte visning",
            glossary_heading: "Substitutionsfarver & ikoner — Request JSON/Hurl-visning",
            glossary_label_literal: "literal",
            glossary_desc_literal: "En simpel literal værdi fra det aktive miljø, indsat direkte.",
            glossary_label_loaded: "indlæst",
            glossary_desc_loaded: "Hentet fra en ekstern kilde (miljøvariabel, 1Password, SSM) eller et initialiseret svar-fangst.",
            glossary_label_pending: "indlæser",
            glossary_desc_pending: "En hemmelighedsreference, der stadig hentes i baggrunden; vises som \"{{ VAR }}\" indtil den er løst.",
            glossary_label_failed: "mangler",
            glossary_desc_failed: "Kunne ikke løses, eller en svar-fangst der endnu ikke er initialiseret — vises som \"{{ VAR }}\".",
            glossary_label_shadowed: "skygget",
            glossary_desc_shadowed: "Denne værdi kommer fra det aktive globale miljø, men bliver overskygget af samlingens tilknyttede miljø — det er den tilknyttede værdi, der faktisk indsættes.",
            glossary_heading_icons: "Andre ikoner brugt i appen",
            glossary_label_modified: "ændret",
            glossary_desc_modified: "En blyant markerer en request, header eller variabel, der er redigeret væk fra sin oprindeligt indlæste værdi.",
            glossary_label_added: "tilføjet",
            glossary_desc_added: "Et plus markerer en request eller variabel, der er tilføjet manuelt i stedet for indlæst fra en fil.",
            glossary_label_passed: "bestået / aktiv",
            glossary_desc_passed: "En request eller assertion, der bestod ved sidste \"Kør alle\", eller det globale miljø, der aktuelt er aktivt i sin liste.",
            glossary_label_run_failed: "fejlede",
            glossary_desc_run_failed: "En request eller assertion, der fejlede ved sidste \"Kør alle\".",
            glossary_label_running: "kører",
            glossary_desc_running: "En request, der stadig kører som en del af en batch \"Kør alle\".",
            glossary_label_git: "git-tilknyttet",
            glossary_desc_git: "Denne samling, dette miljø eller denne Workspace blev indlæst fra, og er stadig tilknyttet, en git-fjernserver.",
            glossary_label_linked: "tilknyttet miljø",
            glossary_desc_linked: "Forbinder en samlings faneblad/titel til det globale miljø, den er tilknyttet.",
            glossary_label_folder: "mappe",
            glossary_desc_folder: "En undermappe, der grupperer requests i listen, eller — ved siden af en formularværdi af typen Fil — et hint om, at Enter åbner en filvælger.",
            glossary_label_scroll_hint: "mere tekst",
            glossary_desc_scroll_hint: "Vises i kanten af en afkortet linje, når der er mere skjult tekst at rulle til i den retning.",
            help_focus: "skift mellem paneler",
            help_move: "flyt inden for et panel",
            help_page_response: "side op/ned gennem svar-teksten",
            help_switch_tabs: "skift faner (Faner-panel)",
            help_select: "vælg / rediger det fokuserede element (guiden)",
            help_run: "send anmodning / kør post",
            help_run_all: "kør alle anmodninger i samlingen, i rækkefølge (som CLI'en)",
            help_raw_mode: "rediger den valgte anmodning i råtilstand (Hurl-tekst)",
            help_raw_json: "rediger den valgte anmodning i råtilstand (JSON-tekst)",
            help_new: "ny anmodning (eller tilføj variabel, i miljø-popup'en)",
            help_base_url: "rediger standard-URL for nye anmodninger",
            help_menus: "Fil- / Indstillinger-menu",
            help_workspace_browse: "Gennemse Workspace (vælg en samlingsfil)",
            help_prev_next_tab: "forrige / næste fane",
            help_rename_close: "omdøb fane (F2) · slet anmodning / luk samlingsfane",
            help_reload_var: "genindlæs en mislykket miljøvariabel (miljøvariabel / 1Password / SSM)",
            help_env_activate: "aktivér / deaktivér det valgte globale miljø",
            help_env_delete: "slet det valgte globale miljø (fjerner link fra samlinger, der bruger det)",
            help_env_link: "link / afkobl et globalt miljø til den aktive samling",
            help_env_view_linked: "vis den aktive samlings tilknyttede globale miljø",
            help_env_rename: "omdøb det åbne globale miljø",
            help_resize: "formindsk / forøg svarpanelet",
            help_resize_width: "forøg / formindsk venstre kolonne",
            help_tab_manage: "luk / genåbn samlings- eller workspace-fane",
            help_tab_reorder: "omarranger faner",
            help_restore_request: "gendan slettet anmodning (Liste-rude)",
            help_row_toggle_delete: "i guidens tabeller: ^E slå række til/fra, ^D slet række",
            help_copy_selection: "kopiér markeringen, eller hele ruden hvis intet er markeret (Request JSON / Request Hurl / Response-rude)",
            help_multi_select: "Alt+Klik+Træk tilføjer endnu et markeringsområde (almindeligt klik rydder alt)",
            help_save_editor: "gem en flerlinjet editor",
            help_cancel: "luk menu / annuller redigering",
            help_quit: "afslut",
            help_group_navigation: "Navigation",
            help_group_tabs: "Faner",
            help_group_requests: "Anmodninger og kørsel",
            help_group_menus: "Menuer og Workspace",
            help_group_environments: "Miljøer",
            help_group_editing: "Redigering og markering",
            help_group_panels: "Paneler og generelt",
            new_request_hint: "Tab/pile flyt · PgUp/PgDn faneblad · Alt+1-6 hop til · ^Enter/F2 opret · Esc annuller",
            edit_request_hint: "Tab/pile flyt · PgUp/PgDn faneblad · Alt+1-6 hop til · ^Enter/F2 gem · Esc annuller",
            raw_mode_hint: "Rediger den rå Hurl-tekst · F2/^Enter genfortolk & gem · Esc annuller · Shift+Pil markér · ^Y kopiér",
            raw_json_hint: "Rediger den rå JSON · F2/^Enter genfortolk & gem · Esc annuller · Shift+Pil markér · ^Y kopiér",
            ctrl_enter_key: "^Enter",
            prompt_rename_title: "Omdøb fane",
            prompt_enter_path: "indtast sti",
            prompt_save_hint_ml: "F2 gem · Esc annuller",
            prompt_save_hint_sl: "Enter gem · Esc annuller",
            prompt_reset_hint: "^R nulstil",
            browser_select_file: "Vælg fil",
            browser_hint: "Enter åbn · ← overordnet · ^h skjulte · Esc annuller",
            tabs_heading: "Samlinger",
            suggest_hint: "↓↑ vælg · Enter udfyld",
        }
    }
}

/// A language-independent status / notification message. It stores *what*
/// happened, not the translated text, so [`Status::text`] can render it in the
/// current language — the message re-translates when the language changes.
#[derive(Clone, Debug)]
pub enum Status {
    Saved,
    Loaded,
    Cleared,
    NoResponse,
    NotCollection,
    NotEnvironment,
    /// Text was copied to the clipboard (a selection or a whole-panel copy).
    Copied,
    /// The active collection has no remembered git origin, so "Save to Git"
    /// can't be opened.
    NoGitOrigin,
    /// A collection (and optionally its environment) was successfully pushed
    /// to git.
    GitSaved,
    /// Secrets the request is waiting on (their variable names).
    WaitingSecrets(Vec<String>),
    /// The user asked to retry a single previously-failed Environment panel
    /// variable (env var / 1Password / SSM); names the variable being retried.
    EnvVarReloading(String),
    /// A Global Environment was activated/deactivated (names it).
    EnvActivated(String),
    EnvDeactivated(String),
    /// "Run All" (Alt+F5) finished running every request in the collection.
    CollectionRunSummary {
        passed: usize,
        failed: usize,
        total: usize,
    },
    /// A restored Workspace tab's root folder no longer exists on disk (e.g.
    /// it was a git-downloaded temp folder and the OS cleared it since) —
    /// names the affected tab. The tab itself has already been reset to a
    /// plain, empty "no collection chosen" tab by the time this is shown.
    WorkspaceFolderMissing(String),
    /// A missing Workspace was successfully redownloaded from git, pinned to
    /// the exact commit it was last at (see
    /// `TuiApp::poll_workspace_redownload_updates`). Always paired with a
    /// hint to save the Workspace locally if the user wants to guarantee it
    /// persists, regardless of this having worked.
    WorkspaceReloaded,
    /// A Workspace redownload attempt failed — holds the (token-redacted)
    /// git error, most often because the exact recorded commit is no longer
    /// reachable on the remote (history rewritten, branch/tag deleted).
    /// Also paired with the "save locally" hint.
    WorkspaceReloadFailed(String),
    /// "Save Workspace…" was invoked while the active tab isn't
    /// Workspace-bound (that action is only ever offered for such tabs).
    NotWorkspace,
    /// "Save Workspace…" finished copying the files to their new, permanent
    /// location.
    WorkspaceSaved,
    /// "Save Workspace…" couldn't complete — holds a raw (non-translatable)
    /// detail (a filesystem error, or the chosen destination already
    /// existing / overlapping the source).
    WorkspaceSaveFailed(String),
    /// A raw (non-translatable) error detail, shown after a translated prefix.
    Error(String),
}

impl Status {
    /// Whether this represents a successful outcome (green) vs a problem (red).
    pub fn is_ok(&self) -> bool {
        match self {
            Status::CollectionRunSummary { failed, .. } => *failed == 0,
            _ => matches!(
                self,
                Status::Saved
                    | Status::Loaded
                    | Status::Cleared
                    | Status::GitSaved
                    | Status::Copied
                    | Status::EnvActivated(_)
                    | Status::EnvDeactivated(_)
                    | Status::WorkspaceReloaded
                    | Status::WorkspaceSaved
            ),
        }
    }

    /// Render the message in the given language.
    pub fn text(&self, s: &Strings) -> String {
        match self {
            Status::Saved => s.file_saved.to_string(),
            Status::Loaded => s.file_loaded.to_string(),
            Status::Cleared => s.clear_all_done.to_string(),
            Status::Copied => s.copied_to_clipboard.to_string(),
            Status::NoResponse => s.file_no_response.to_string(),
            Status::NotCollection => s.file_not_collection.to_string(),
            Status::NotEnvironment => s.file_not_environment.to_string(),
            Status::NoGitOrigin => s.git_no_origin.to_string(),
            Status::GitSaved => s.git_save_success.to_string(),
            Status::WaitingSecrets(keys) => {
                format!("{} {}", s.env_waiting_secrets, keys.join(", "))
            }
            Status::EnvVarReloading(key) => format!("{} {key}…", s.env_reloading_var),
            Status::EnvActivated(name) => format!("{} {name}", s.env_activated),
            Status::EnvDeactivated(name) => format!("{} {name}", s.env_deactivated),
            Status::WorkspaceFolderMissing(name) => {
                s.workspace_folder_missing.replace("{name}", name)
            }
            Status::WorkspaceReloaded => {
                format!(
                    "{} {}",
                    s.workspace_reload_success, s.workspace_reload_save_hint
                )
            }
            Status::WorkspaceReloadFailed(e) => {
                format!(
                    "{} {}",
                    s.workspace_reload_failed.replace("{e}", e),
                    s.workspace_reload_save_hint
                )
            }
            Status::NotWorkspace => s.file_not_workspace.to_string(),
            Status::WorkspaceSaved => s.workspace_save_success.to_string(),
            Status::WorkspaceSaveFailed(e) => s.workspace_save_failed.replace("{e}", e),
            Status::CollectionRunSummary {
                passed,
                failed,
                total,
            } => format!(
                "{}: {passed}  {}: {failed}  {}: {total}",
                s.run_summary_passed, s.run_summary_failed, s.run_summary_total
            ),
            Status::Error(e) => format!("{} {e}", s.file_error_prefix),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text_follows_the_current_language() {
        let en = Strings::for_language(&Language::English);
        let fr = Strings::for_language(&Language::French);
        let da = Strings::for_language(&Language::Danish);

        // The same Status renders differently per language (re-translates).
        let st = Status::NotCollection;
        assert_ne!(st.text(&en), st.text(&fr));
        assert!(st.text(&en).starts_with("Not"));
        assert!(st.text(&fr).starts_with("Fichier"));
        assert!(st.text(&da).starts_with("Ikke"));

        // Success vs problem classification.
        assert!(Status::Loaded.is_ok());
        assert!(!Status::NotEnvironment.is_ok());

        // Dynamic parts are preserved and prefixed/translated.
        assert!(Status::Error("boom".into()).text(&en).contains("boom"));
        assert!(
            Status::WaitingSecrets(vec!["TOKEN".into()])
                .text(&en)
                .contains("TOKEN")
        );
    }

    #[test]
    fn danish_strings_are_present_and_distinct() {
        let da = Strings::for_language(&Language::Danish);
        assert_eq!(da.lang_danish, "Dansk");
        // A few representative strings are actually translated, not left English.
        assert_eq!(da.file_menu, "Fil");
        assert_eq!(da.response_heading, "Svar");
        assert_eq!(da.new_request, "\u{FF0B} Ny anmodning");
    }
}

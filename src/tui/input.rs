use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders};
use ratatui_explorer::{FileExplorer, FileExplorerBuilder, Theme as ExplorerTheme};

use crate::collection::Collection;
use crate::environment::{PendingEnvSecrets, spawn_resolution_many};
use crate::hurl::{FormField, FormFieldKind, HurlEntry, METHODS};
use crate::i18n::{Language, Status, Strings};
use crate::persistence::{
    self, PendingWorkspaceReload, PersistedEnv, PersistedState, PersistedTab,
};
use crate::request::{self, AppVars, build_request_json};

use super::app::*;
use super::clipboard::copy_to_clipboard;
use super::editor::*;
use super::new_request::*;
use super::remote::*;
use super::selection;

impl TuiApp {
    pub(crate) fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if self.overlay.is_some() {
            self.on_key_overlay(key);
        } else {
            self.on_key_normal(key);
        }
    }

    /// Drive panel-scoped text selection in the Request JSON / Response
    /// panels. The app captures the mouse itself (see `tui::run`) precisely
    /// so a drag can be confined to whichever panel it started in — a click
    /// outside both panels' cached text areas clears any existing selection
    /// instead of starting a new one. Most overlays don't use the mouse and
    /// ignore these events entirely, except the Raw Mode / Raw JSON Mode
    /// editors (see `on_mouse_raw_text_editor`), which support their own
    /// click-drag selection scoped to their own text.
    ///
    /// A plain click-drag replaces the *entire* selection (clearing any
    /// additional regions too). Alt+Click+Drag instead *adds* a new region:
    /// whatever was the active region is finalized into `extra_selections`
    /// and a fresh one starts at the click point, so an arbitrary number of
    /// (possibly cross-panel) regions can be built up one at a time. Alt
    /// isn't used by terminals for their own native-selection bypass (that's
    /// Shift) or hyperlink-opening (that's usually Ctrl), so it's the one
    /// modifier that reliably reaches the app in both cases.
    pub(crate) fn on_mouse(&mut self, ev: MouseEvent) {
        if self.overlay_is_raw_text_editor() {
            self.on_mouse_raw_text_editor(ev);
            return;
        }
        if self.overlay.is_some() {
            return;
        }
        let point = Position::new(ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // A click on either panel's scrollbar jumps straight to that
                // row's position in the content and starts a drag, without
                // touching any existing text selection.
                if let Some(pane) = self.scrollbar_pane_at(point) {
                    self.scrollbar_drag = Some(pane);
                    self.set_scroll_for_row(pane, ev.row);
                    return;
                }
                let pane = if self.main_text_area.contains(point) {
                    Some(Pane::Main)
                } else if self.resp_text_area.contains(point) {
                    Some(Pane::Response)
                } else {
                    None
                };
                self.pending_autoscroll = None;
                if ev.modifiers.contains(KeyModifiers::ALT) {
                    if let Some(prev) = self.text_selection.take() {
                        self.extra_selections.push(prev);
                    }
                } else {
                    self.extra_selections.clear();
                }
                self.text_selection = pane.and_then(|pane| {
                    let (area, scroll) = self.panel_area_scroll(pane)?;
                    let wrap = self.panel_wrap(pane)?;
                    let pos = selection::point_to_textpos((ev.column, ev.row), area, scroll, wrap);
                    Some(TextSelection {
                        pane,
                        anchor: pos,
                        cursor: pos,
                    })
                });
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(pane) = self.scrollbar_drag {
                    self.set_scroll_for_row(pane, ev.row);
                    return;
                }
                self.drag_selection_to((ev.column, ev.row));
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.scrollbar_drag.take().is_some() {
                    return;
                }
                self.pending_autoscroll = None;
                self.copy_selection_to_clipboard();
            }
            _ => {}
        }
    }

    /// Which panel (if any) has its scrollbar under `point`.
    fn scrollbar_pane_at(&self, point: Position) -> Option<Pane> {
        if self.main_scrollbar_area.contains(point) {
            Some(Pane::Main)
        } else if self.resp_scrollbar_area.contains(point) {
            Some(Pane::Response)
        } else {
            None
        }
    }

    /// Set `pane`'s scroll offset to whatever position along its scrollbar
    /// track `row` maps to (clamped to the track's own bounds), so clicking
    /// or dragging anywhere in the track jumps/scrolls proportionally —
    /// mirroring how a native scrollbar behaves.
    fn set_scroll_for_row(&mut self, pane: Pane, row: u16) {
        let (area, max_scroll) = match pane {
            Pane::Main => (self.main_scrollbar_area, self.main_max_scroll),
            Pane::Response => (self.resp_scrollbar_area, self.resp_max_scroll),
            _ => return,
        };
        if area.height == 0 || max_scroll == 0 {
            return;
        }
        let track = area.height.saturating_sub(1).max(1) as f64;
        let rel = row
            .saturating_sub(area.y)
            .min(area.height.saturating_sub(1)) as f64;
        let scroll = ((rel / track) * max_scroll as f64).round() as u16;
        match pane {
            Pane::Main => self.main_scroll = scroll.min(max_scroll),
            Pane::Response => self.resp_scroll = scroll.min(max_scroll),
            _ => {}
        }
    }

    /// Whether the current overlay is Raw Mode's or Raw JSON Mode's editor —
    /// the only overlays that accept mouse events (for their own click-drag
    /// text selection).
    fn overlay_is_raw_text_editor(&self) -> bool {
        matches!(
            self.overlay,
            Some(Overlay::Prompt {
                kind: PromptKind::Raw(_) | PromptKind::RawJson(_),
                ..
            })
        )
    }

    /// Drive click-drag text selection inside Raw Mode's / Raw JSON Mode's
    /// editor (the actual Hurl or JSON text opened with Shift+H / Shift+J),
    /// mirroring the Main/Response panels' mouse-selection behavior but
    /// scoped to `prompt_editor_area` and the editor's own
    /// `sel_anchor`/`(row, col)` cursor instead of a `TextSelection`/
    /// `PanelWrap`. A click outside the editor's text area (e.g. on the
    /// border or hint) is ignored rather than starting a selection at a
    /// nonsensical position.
    fn on_mouse_raw_text_editor(&mut self, ev: MouseEvent) {
        let area = self.prompt_editor_area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        let Some(Overlay::Prompt { editor, .. }) = self.overlay.as_mut() else {
            return;
        };
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left)
                if area.contains(Position::new(ev.column, ev.row)) =>
            {
                let (row, col) = editor.point_to_row_col((ev.column, ev.row), area);
                editor.row = row;
                editor.col = col;
                editor.sel_anchor = Some((row, col));
            }
            MouseEventKind::Drag(MouseButton::Left) if editor.sel_anchor.is_some() => {
                let (row, col) = editor.point_to_row_col((ev.column, ev.row), area);
                editor.row = row;
                editor.col = col;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(text) = editor.selected_text() {
                    copy_to_clipboard(&text);
                    self.status = Some(Status::Copied);
                }
            }
            _ => {}
        }
    }

    /// The panel's own Rect and current (wrapped-row) scroll offset, for
    /// whichever of Main/Response `pane` is — `None` for any other pane.
    fn panel_area_scroll(&self, pane: Pane) -> Option<(Rect, u16)> {
        match pane {
            Pane::Main => Some((self.main_text_area, self.main_scroll)),
            Pane::Response => Some((self.resp_text_area, self.resp_scroll)),
            _ => None,
        }
    }

    /// The panel's cached line/wrap structure (see `wrapcache::PanelWrap`),
    /// for whichever of Main/Response `pane` is — `None` if that panel has
    /// no content yet (nothing drawn/no response) or `pane` is neither.
    fn panel_wrap(&self, pane: Pane) -> Option<&super::wrapcache::PanelWrap> {
        match pane {
            Pane::Main => self.main_wrap.as_ref(),
            Pane::Response => self.resp_wrap.as_ref(),
            _ => None,
        }
    }

    /// The entire (unscrolled, unwrapped) text of whichever of Main
    /// (Request JSON) / Response `pane` is — `None` if that panel is
    /// neither, has no content cached yet, or is simply empty. Backs the
    /// "copy the whole panel" fallback that kicks in when `y` is pressed
    /// with no active selection. For the Main panel, any shadow-warning
    /// icons (see `main_shadow_icon_positions`) are stripped out first —
    /// they're a purely visual annotation, so a copied/pasted request must
    /// never actually contain one.
    pub(crate) fn whole_panel_text(&self, pane: Pane) -> Option<String> {
        let text = self.panel_wrap(pane)?.source();
        if text.is_empty() {
            return None;
        }
        if pane == Pane::Main {
            Some(selection::strip_positions(
                text,
                &self.main_shadow_icon_positions,
            ))
        } else {
            Some(text.to_string())
        }
    }

    /// Continue an active selection drag to `point`. When the drag has
    /// moved past the panel's own top/bottom edge, this doesn't just clamp
    /// the point back inside (stalling the selection there): it starts
    /// auto-scrolling that panel in that direction, extending the selection
    /// a full line at a time so it keeps growing for as long as the drag
    /// stays outside the bounds — even without further mouse movement, since
    /// `pending_autoscroll` is also ticked once per idle main-loop iteration
    /// (see `tui::run`).
    fn drag_selection_to(&mut self, point: (u16, u16)) {
        let Some(sel) = self.text_selection else {
            return;
        };
        let pane = sel.pane;
        let Some((area, scroll)) = self.panel_area_scroll(pane) else {
            return;
        };
        let (_, row) = point;
        if area.height > 0 && row < area.y {
            self.pending_autoscroll = Some((pane, -1));
            self.autoscroll_tick();
            return;
        }
        if area.height > 0 && row >= area.y.saturating_add(area.height) {
            self.pending_autoscroll = Some((pane, 1));
            self.autoscroll_tick();
            return;
        }
        self.pending_autoscroll = None;
        let Some(wrap) = self.panel_wrap(pane) else {
            return;
        };
        let pos = selection::point_to_textpos(point, area, scroll, wrap);
        if let Some(sel) = self.text_selection.as_mut() {
            sel.cursor = pos;
        }
    }

    /// One "tick" of auto-scrolling a drag held past the panel's vertical
    /// bounds: scrolls one row in the pending direction and extends the
    /// selection cursor to the newly revealed edge line (its start when
    /// scrolling up into view, its end when scrolling down into view) — a
    /// whole line at a time, as intended for a drag that's left the panel's
    /// visible area entirely. Once the content's own top/bottom is reached
    /// (scrolling can't go any further) but the drag is still held past the
    /// edge, it instead keeps snapping the cursor to the very first/last
    /// line's full extent, so that boundary line ends up entirely
    /// highlighted rather than left wherever the drag last was inside the
    /// panel.
    pub(crate) fn autoscroll_tick(&mut self) {
        let Some((pane, dir)) = self.pending_autoscroll else {
            return;
        };
        if self.text_selection.is_none() {
            self.pending_autoscroll = None;
            return;
        }
        let (area, max_scroll) = match pane {
            Pane::Main => (self.main_text_area, self.main_max_scroll),
            Pane::Response => (self.resp_text_area, self.resp_max_scroll),
            _ => {
                self.pending_autoscroll = None;
                return;
            }
        };
        let scroll_field = match pane {
            Pane::Main => &mut self.main_scroll,
            Pane::Response => &mut self.resp_scroll,
            _ => unreachable!(),
        };
        let new_scroll = if dir < 0 {
            scroll_field.saturating_sub(1)
        } else {
            (*scroll_field + 1).min(max_scroll)
        };
        let reached_bound = new_scroll == *scroll_field;
        *scroll_field = new_scroll;
        let Some(wrap) = self.panel_wrap(pane) else {
            return;
        };
        let edge_row = if reached_bound {
            if dir < 0 {
                0
            } else {
                wrap.total_rows().saturating_sub(1)
            }
        } else if dir < 0 {
            new_scroll as u32
        } else {
            (new_scroll as u32 + area.height as u32).saturating_sub(1)
        };
        let col = if dir < 0 { 0 } else { usize::MAX };
        let pos = wrap.row_col_to_textpos(edge_row, col);
        if let Some(sel) = self.text_selection.as_mut() {
            sel.cursor = pos;
        }
    }

    /// Shift+Arrow: move the *end* of the active selection by one character
    /// (Left/Right, crossing line boundaries) or one logical line (Up/Down,
    /// keeping the same column where possible) — a keyboard-only way to
    /// adjust a selection already started with the mouse, without redoing
    /// the whole drag. Scrolls the panel to keep the moved end in view.
    pub(crate) fn extend_selection(&mut self, dir: KeyCode) {
        let Some(sel) = self.text_selection else {
            return;
        };
        let Some(wrap) = self.panel_wrap(sel.pane) else {
            return;
        };
        let mut pos = sel.cursor;
        match dir {
            KeyCode::Left => {
                if pos.col > 0 {
                    pos.col -= 1;
                } else if pos.line > 0 {
                    pos.line -= 1;
                    pos.col = wrap.line_char_len(pos.line).saturating_sub(1);
                }
            }
            KeyCode::Right => {
                let len = wrap.line_char_len(pos.line);
                if pos.col + 1 < len {
                    pos.col += 1;
                } else if pos.line + 1 < wrap.line_count() {
                    pos.line += 1;
                    pos.col = 0;
                }
            }
            KeyCode::Up => {
                if pos.line > 0 {
                    pos.line -= 1;
                    pos.col = pos.col.min(wrap.line_char_len(pos.line).saturating_sub(1));
                }
            }
            KeyCode::Down => {
                if pos.line + 1 < wrap.line_count() {
                    pos.line += 1;
                    pos.col = pos.col.min(wrap.line_char_len(pos.line).saturating_sub(1));
                }
            }
            _ => return,
        }
        if let Some(sel) = self.text_selection.as_mut() {
            sel.cursor = pos;
        }
        self.scroll_selection_cursor_into_view();
    }

    /// After moving the selection's cursor end (Shift+Arrow), nudge the
    /// owning panel's scroll so that end stays visible, exactly like a text
    /// editor's cursor never being allowed to scroll off-screen.
    fn scroll_selection_cursor_into_view(&mut self) {
        let Some(sel) = self.text_selection else {
            return;
        };
        let Some((area, _)) = self.panel_area_scroll(sel.pane) else {
            return;
        };
        if area.height == 0 {
            return;
        }
        let Some(wrap) = self.panel_wrap(sel.pane) else {
            return;
        };
        let (row, _) = wrap.textpos_to_row_col(sel.cursor);
        let max_scroll = match sel.pane {
            Pane::Main => self.main_max_scroll,
            Pane::Response => self.resp_max_scroll,
            _ => return,
        };
        let scroll_field = match sel.pane {
            Pane::Main => &mut self.main_scroll,
            Pane::Response => &mut self.resp_scroll,
            _ => return,
        };
        if row < *scroll_field as u32 {
            *scroll_field = row as u16;
        } else if row >= *scroll_field as u32 + area.height as u32 {
            *scroll_field = (row + 1).saturating_sub(area.height as u32) as u16;
        }
        *scroll_field = (*scroll_field).min(max_scroll);
    }

    /// Concatenate the extracted text of every active selection region —
    /// the active one (`text_selection`) plus any additional Alt+Click+Drag
    /// regions in `extra_selections` — ordered by where each region actually
    /// starts in the text (panel order, then position within it), not by
    /// the order the regions were drawn in. So dragging the end of a body
    /// first and then Alt+Click+Dragging the start still copies start-then-
    /// end. Pure (no clipboard I/O), so the multi-region logic itself is
    /// directly unit-testable; `None` when there's nothing selected anywhere.
    pub(crate) fn concatenated_selection_text(&self) -> Option<String> {
        let mut sels: Vec<&TextSelection> = self
            .extra_selections
            .iter()
            .chain(self.text_selection.iter())
            .collect();
        sels.sort_by_key(|sel| (pane_rank(sel.pane), sel.anchor.min(sel.cursor)));

        let mut parts = Vec::new();
        for sel in sels {
            let Some(wrap) = self.panel_wrap(sel.pane) else {
                continue;
            };
            // Only the Main panel can ever contain a shadow-warning icon
            // (see `main_shadow_icon_positions`) — exclude it from the
            // copied text so a dragged selection never carries a stray "!"
            // into a pasted request, same as the whole-panel copy fallback.
            let exclude = (sel.pane == Pane::Main).then_some(&self.main_shadow_icon_positions);
            if let Some(text) = selection::extract_text(sel.anchor, sel.cursor, wrap, exclude) {
                parts.push(text);
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }

    /// Copy every active text selection region to the clipboard via OSC 52
    /// (see `concatenated_selection_text`). Shared by the mouse-release
    /// handler (drag-to-select) and the `y` keyboard shortcut, the latter
    /// existing because OSC 52 write-back isn't picked up by every terminal
    /// emulator / multiplexer config, so users need an explicit, repeatable
    /// way to retry the copy without having to redo every drag.
    ///
    /// With nothing selected, `y` falls back to copying the *whole* content
    /// of whichever of the Main (Request JSON) / Response panels currently
    /// has focus — so the whole request/response can be grabbed without
    /// first having to drag-select every line of a possibly huge body.
    pub(crate) fn copy_selection_to_clipboard(&mut self) {
        if let Some(text) = self.concatenated_selection_text() {
            copy_to_clipboard(&text);
            self.status = Some(Status::Copied);
        } else if let Some(text) = self.whole_panel_text(self.focus) {
            copy_to_clipboard(&text);
            self.status = Some(Status::Copied);
        }
    }

    pub(crate) fn on_key_overlay(&mut self, key: KeyEvent) {
        let Some(overlay) = self.overlay.take() else {
            return;
        };
        match overlay {
            Overlay::Help(tab) => self.help_key_handler(key, tab),
            Overlay::RemoteGit(w) => self.on_key_remote(w, key),
            Overlay::GitSave(w) => self.on_key_git_save(w, key),
            Overlay::EnvPopup(popup) => self.on_key_env_popup(popup, key),
            Overlay::EnvLinkPicker(picker) => self.on_key_env_link_picker(picker, key),
            Overlay::EnvCollision(collision) => self.on_key_env_collision(*collision, key),
            Overlay::ThemeEditor(state) => self.on_key_theme_editor(state, key),
            Overlay::WorkspacePicker(picker) => self.on_key_workspace_picker(picker, key),
            Overlay::CloseGitWorkspace { idx, path, sel } => {
                self.close_git_workspace_key_handler(key, idx, path, sel)
            }
            Overlay::WorkspaceGitSaveUnsaved { ci, sel } => {
                self.workspace_git_save_unsaved_key_handler(key, ci, sel)
            }
            Overlay::WorkspaceSwitchUnsaved { ci, target, sel } => {
                self.workspace_switch_unsaved_key_handler(key, ci, target, sel)
            }
            Overlay::WorkspaceReloadConfirm { idx, reload, sel } => {
                self.workspace_reload_confirm_key_handler(key, idx, reload, sel)
            }
            // No key handling while a redownload is running — the user just
            // waits (mirrors `RemoteStage::Loading`, which is also inert).
            Overlay::WorkspaceReloadLoading { .. } => {}
            Overlay::WorkspaceStorageChoice {
                repo,
                name,
                origin,
                sel,
            } => self.workspace_storage_choice_key_handler(key, repo, name, origin, sel),
            Overlay::FileMenu(sel) => self.file_menu_key_handler(key, sel),
            Overlay::FileLoadMenu(sel) => self.file_load_menu_key_handler(key, sel),
            Overlay::FileSaveMenu(sel) => self.file_save_menu_key_handler(key, sel),
            Overlay::FileLoadSource(kind, sel) => self.file_load_source_key_handler(key, kind, sel),
            Overlay::FileSaveDest(kind, sel) => self.file_save_dest_key_handler(key, kind, sel),
            Overlay::Options(sel) => self.options_key_handler(key, sel),
            Overlay::Preferences(sel) => self.preferences_key_handler(key, sel),
            Overlay::Confirm { action, sel } => self.confirm_key_handler(key, action, sel),
            Overlay::LanguageMenu(sel) => self.language_menu_key_handler(key, sel),
            Overlay::RequestViewMenu(sel) => self.request_view_menu_key_handler(key, sel),
            Overlay::Prompt {
                kind,
                editor,
                title,
                mask,
                reset_to,
                secret_intact,
                secret_checkbox,
            } => self.prompt_key_handler(
                key,
                kind,
                editor,
                title,
                mask,
                reset_to,
                secret_intact,
                secret_checkbox,
            ),
            Overlay::EnvVarForm(form) => self.env_var_form_key_handler(key, form),
            Overlay::Browser(action, ex) => self.browser_key_handler(key, action, ex),
            Overlay::NewRequest(form) => self.new_request_key_handler(key, form),
        }
    }

    pub(crate) fn on_key_normal(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            // Esc dismisses every active mouse text selection first (the
            // active one and any additional Alt+Click+Drag regions), so
            // nothing lingers highlighted; only takes effect when there is
            // at least one, so it doesn't shadow any other key's behaviour.
            KeyCode::Esc if self.has_any_selection() => self.clear_selections(),
            // `y` (vim-style "yank") copies to the clipboard on demand — an
            // explicit fallback for terminals where the automatic
            // copy-on-mouse-release OSC 52 write isn't picked up (e.g. no
            // passthrough configured in tmux/screen, or the terminal simply
            // ignores OSC 52). With a selection active, it (re-)copies just
            // that; with none, it copies the *whole* Request JSON / Response
            // panel, whichever currently has focus — `can_copy()` is the
            // single source of truth for "would `y` do anything right now",
            // shared with the footer hint so they can't drift apart.
            KeyCode::Char('y') if self.can_copy() => self.copy_selection_to_clipboard(),
            // Shift+Arrow moves the *end* of an active selection, letting
            // the user fine-tune (or start extending, one line/char at a
            // time) a selection begun with the mouse, without redoing the
            // whole drag.
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                if shift && !ctrl && self.text_selection.is_some() =>
            {
                self.extend_selection(key.code);
            }
            // Ctrl+W closes the active collection tab (an unambiguous alias for
            // `x`, usable regardless of which pane has focus).
            KeyCode::Char('w') if ctrl => self.close_active_tab(),
            // Reopens the most recently closed tab. Deliberately a plain
            // unmodified key rather than a Ctrl+Shift combo: terminal emulators
            // commonly intercept Ctrl+Shift+T themselves (as "new tab") before
            // it ever reaches the app. When the Requests list has focus, `u`
            // instead restores the most recently deleted request in the active
            // collection (mirroring how `x` deletes a request there instead of
            // closing the tab) — see `restore_deleted_request`.
            KeyCode::Char('u') if self.focus == Pane::List => self.restore_deleted_request(),
            // `u` in the Global Environments panel reopens the most recently
            // deleted environment (mirroring how `x` deletes one there).
            KeyCode::Char('u') if self.focus == Pane::GlobalEnv => self.restore_deleted_env(),
            KeyCode::Char('u') => self.reopen_closed_tab(),
            // Ctrl+Shift+Left/Right reorders the active tab (index 0, the
            // built-in Request tab, never moves).
            KeyCode::Left if ctrl && shift => self.move_active_tab(false),
            KeyCode::Right if ctrl && shift => self.move_active_tab(true),
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Tab => self.cycle_focus(true),
            KeyCode::BackTab => self.cycle_focus(false),
            KeyCode::Char('f') => self.overlay = Some(Overlay::FileMenu(0)),
            KeyCode::Char('s') => {
                self.overlay = Some(Overlay::Options(0));
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.overlay = Some(Overlay::Help(0));
                self.help_scroll = 0;
            }
            KeyCode::Char('b') => self.open_prompt_baseurl(),
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.response_pct = self.response_pct.saturating_sub(5).max(15);
                self.save_state();
            }
            KeyCode::Char('-') => {
                self.response_pct = (self.response_pct + 5).min(75);
                self.save_state();
            }
            KeyCode::Char('>') => {
                self.list_width = (self.list_width + 2).min(80);
                self.save_state();
            }
            KeyCode::Char('<') => {
                self.list_width = self.list_width.saturating_sub(2).max(20);
                self.save_state();
            }
            KeyCode::Char('[') => self.cycle_tab(false),
            KeyCode::Char(']') => self.cycle_tab(true),
            KeyCode::PageUp => self.cycle_tab(false),
            KeyCode::PageDown => self.cycle_tab(true),
            // Ctrl+Left/Right: a third prev/next-tab alias, easier to type
            // than PageUp/PageDown on many keyboards. Safe from any pane —
            // Ctrl+Left/Right's other meaning (jump to start/end of a text
            // field) only ever applies inside overlays, which are handled by
            // a completely separate key-handling path from this one.
            KeyCode::Left if ctrl => self.cycle_tab(false),
            KeyCode::Right if ctrl => self.cycle_tab(true),
            // F2 on the environments panel renames the selected Global
            // Environment; elsewhere it renames the active tab. The panel arm
            // is listed first so it wins when that panel is focused (otherwise
            // the tab-rename shortcut would shadow it).
            KeyCode::F(2) if self.focus == Pane::GlobalEnv && !self.global_envs.is_empty() => {
                if let Some(env_id) = self.global_envs.get(self.global_env_idx).map(|e| e.id) {
                    self.open_prompt_rename_env(env_id);
                }
            }
            // F2 renames the active tab (matches the common OS convention).
            KeyCode::F(2) if self.active_tab != 0 => self.open_prompt_rename(),
            KeyCode::Char('x') if self.focus == Pane::List => self.delete_selected_request(),
            // 'x' in the Global Environments panel deletes the selected
            // environment (any collections linked to it become unlinked).
            // Guarded by the confirm-on-delete-env preference; when it's off,
            // delete straight away (still undoable with `u`).
            KeyCode::Char('x') if self.focus == Pane::GlobalEnv && !self.global_envs.is_empty() => {
                if self.confirm_on_delete_env {
                    self.overlay = Some(Overlay::Confirm {
                        action: ConfirmAction::DeleteEnv(self.global_env_idx),
                        sel: 1,
                    });
                } else {
                    self.delete_global_env(self.global_env_idx);
                }
            }
            KeyCode::Char('x') if self.active_tab != 0 => self.close_active_tab(),
            // 'm' / 'c' move / copy the highlighted request in a Workspace tab
            // to another collection file in the same workspace (a no-op unless
            // a request row of a workspace tab is highlighted). The workspace
            // picker then chooses the destination and writes it to disk.
            KeyCode::Char('m') if self.focus == Pane::List => self.start_workspace_transfer(true),
            KeyCode::Char('c') if self.focus == Pane::List => self.start_workspace_transfer(false),
            // 'a' toggles activation of the selected Global Environment (at
            // most one may be active — activating one deactivates any other).
            KeyCode::Char('a') if self.focus == Pane::GlobalEnv => {
                self.toggle_activate_env(self.global_env_idx);
            }
            // 'p' in the Requests list links/unlinks a Global Environment to
            // the active collection.
            KeyCode::Char('p') if self.focus == Pane::List => {
                let ci = self.active_tab;
                let linked = self.collections[ci].linked_env_id;
                let sel = linked
                    .and_then(|id| self.global_envs.iter().position(|e| e.id == id))
                    .map(|i| i + 1)
                    .unwrap_or(0);
                self.overlay = Some(Overlay::EnvLinkPicker(EnvLinkPicker { ci, sel }));
            }
            // 'v' views the active collection's Linked Environment (if any)
            // in the same entries popup used by the Global Environments
            // list. Deliberately available from every pane (not just the
            // Tabs bar) since which environment a collection substitutes
            // from is relevant no matter what's focused; it's a no-op when
            // nothing is linked.
            KeyCode::Char('v') => {
                if let Some(env_id) = self.collections[self.active_tab].linked_env_id {
                    self.overlay = Some(Overlay::EnvPopup(EnvPopupState::new(env_id)));
                }
            }
            // 'w' (re)opens the Workspace file-tree popup for the active
            // tab, so the user can choose a different collection from the
            // same folder — available from any pane, same rationale as 'v'.
            // A no-op on a tab that isn't Workspace-bound (creating a new
            // Workspace tab is only done via File → Load → "(W)orkspace…").
            KeyCode::Char('w') => self.open_workspace_picker_for_active_tab(),
            // Backspace is a shortcut for Enter on the Requests list's "up"
            // row: go up a folder without needing it highlighted first.
            KeyCode::Backspace
                if self.focus == Pane::List
                    && !self.collections[self.active_tab].folder.is_empty() =>
            {
                self.list_folder_up(self.active_tab);
            }
            KeyCode::Char('n') => {
                let s = Strings::for_language(&self.language);
                let names: Vec<String> = self
                    .collections
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        if i == 0 {
                            s.tab_request.to_string()
                        } else {
                            c.name.clone()
                        }
                    })
                    .collect();
                let file_root = self.collections[self.active_tab]
                    .path
                    .as_ref()
                    .and_then(|p| p.parent().map(std::path::PathBuf::from));
                let mut form = NewReq::new(
                    self.vars.base_url.clone(),
                    names,
                    self.active_tab,
                    file_root,
                );
                // Prefill the Name field with the folder currently being
                // browsed, so saving without editing it creates the request
                // right where the user is looking, e.g. "Auth/" + typed name.
                let folder = &self.collections[self.active_tab].folder;
                if !folder.is_empty() {
                    form.name = Editor::new(&format!("{}/", folder.join("/")), false);
                }
                self.overlay = Some(Overlay::NewRequest(Box::new(form)));
            }
            // Shift+H opens Raw Mode (the actual Hurl text of the selected
            // request) as a secondary editing path to the Edit Request wizard.
            KeyCode::Char('H')
                if matches!(self.focus, Pane::List | Pane::Main)
                    && !self.collections[self.active_tab].entries.is_empty() =>
            {
                self.open_raw_mode_editor(self.active_tab);
            }
            // Shift+J opens Raw JSON Mode (the same JSON the Main panel
            // previews, in the JSON view) — the JSON-text counterpart to
            // Shift+H's Hurl-text editor, a third editing path alongside the
            // Edit Request wizard (Enter) and Raw Mode (Shift+H).
            KeyCode::Char('J')
                if matches!(self.focus, Pane::List | Pane::Main)
                    && !self.collections[self.active_tab].entries.is_empty() =>
            {
                self.open_raw_json_editor(self.active_tab);
            }
            // Alt+F5 runs every request in the active collection in order, in
            // one Hurl execution — the same "batch" behaviour as the CLI,
            // consistent with plain F5 for a single request.
            KeyCode::F(5) if alt => self.run_all_entries(self.active_tab),
            KeyCode::F(5) => self.primary_send(),
            KeyCode::Enter if ctrl => self.primary_send(),
            KeyCode::Enter => self.on_enter(),
            // Ctrl+↑/↓ scrolls the Response panel by a full page (its
            // current visible height) instead of one line at a time, making
            // it much quicker to move through large responses.
            KeyCode::Up if ctrl && self.focus == Pane::Response => {
                self.nav(-(self.resp_text_area.height.max(1) as i32));
            }
            KeyCode::Down if ctrl && self.focus == Pane::Response => {
                self.nav(self.resp_text_area.height.max(1) as i32);
            }
            KeyCode::Up | KeyCode::Char('k') => self.nav(-1),
            KeyCode::Down | KeyCode::Char('j') => self.nav(1),
            KeyCode::Left | KeyCode::Char('h') if self.focus == Pane::Tabs => self.cycle_tab(false),
            KeyCode::Right | KeyCode::Char('l') if self.focus == Pane::Tabs => self.cycle_tab(true),
            KeyCode::Left | KeyCode::Char('h') if self.focus == Pane::List => {
                self.scroll_list_h(-4)
            }
            KeyCode::Right | KeyCode::Char('l') if self.focus == Pane::List => {
                // In a Workspace tab's file-tree, Right opens whatever is
                // highlighted (descend into a folder, or open/expand a
                // collection), matching a file browser; otherwise it
                // horizontally scrolls the selected request's URL.
                let ci = self.active_tab;
                let col = &self.collections[ci];
                let row = col
                    .is_workspace()
                    .then(|| col.ws_rows().into_iter().nth(col.list_cursor))
                    .flatten();
                match row {
                    Some(crate::collection::WsRow::Folder(name)) => {
                        self.workspace_folder_down(ci, name)
                    }
                    Some(crate::collection::WsRow::Collection {
                        path, open: false, ..
                    }) => self.activate_workspace_collection(ci, path),
                    _ => self.scroll_list_h(4),
                }
            }
            KeyCode::Left | KeyCode::Char('h') if self.focus == Pane::GlobalEnv => {
                self.scroll_env_h(-4)
            }
            KeyCode::Right | KeyCode::Char('l') if self.focus == Pane::GlobalEnv => {
                self.scroll_env_h(4)
            }
            _ => {}
        }
    }

    pub(crate) fn primary_send(&mut self) {
        self.run_entry(self.active_tab);
    }

    /// Scroll the selected entry's URL horizontally in the collections list so
    /// long request URLs can be read to the end. Clamped so scrolling stops once
    /// the end of the name has come into view (no scrolling into blank space).
    pub(crate) fn scroll_list_h(&mut self, delta: i32) {
        let ci = self.active_tab;
        let sel = self.collections[ci].selected_entry;
        // Measure the SUBSTITUTED display length (what the list actually shows),
        // so scrolling stops once the substituted URL's end is in view.
        let env = self.effective_env(ci);
        let smap = crate::request::subst_map(&self.collections[ci], env.as_ref());
        let len = self.collections[ci]
            .entries
            .get(sel)
            .map(|e| crate::request::subst_display(&e.url, &smap).chars().count())
            .unwrap_or(0);
        self.list_hscroll = clamp_hscroll(self.list_hscroll, delta, len, self.list_scroll_w.get());
    }

    /// Scroll the selected Global Environment's name horizontally in the
    /// Global Environments list, clamped the same way as the collections list.
    pub(crate) fn scroll_env_h(&mut self, delta: i32) {
        let len = self
            .global_envs
            .get(self.global_env_idx)
            .map(|e| e.name.chars().count())
            .unwrap_or(0);
        self.global_env_hscroll = clamp_hscroll(
            self.global_env_hscroll,
            delta,
            len,
            self.global_env_scroll_w.get(),
        );
    }

    pub(crate) fn panes(&self) -> Vec<Pane> {
        // Reading order: top-left (List/Collections) → top-right (Main/Request
        // JSON) → bottom-left (Env/Environments) → bottom-right (Response),
        // with the Tabs bar first since it sits above everything else.
        vec![
            Pane::Tabs,
            Pane::List,
            Pane::Main,
            Pane::GlobalEnv,
            Pane::Response,
        ]
    }

    pub(crate) fn cycle_focus(&mut self, forward: bool) {
        let panes = self.panes();
        let cur = panes.iter().position(|p| *p == self.focus).unwrap_or(0);
        let n = panes.len();
        let next = if forward {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
        self.focus = panes[next];
    }

    pub(crate) fn cycle_tab(&mut self, forward: bool) {
        let total = self.collections.len();
        self.active_tab = if forward {
            (self.active_tab + 1) % total
        } else {
            (self.active_tab + total - 1) % total
        };
        self.main_scroll = 0;
        self.list_hscroll = 0;
        // Global Environments selection/scroll state is independent of the
        // active collection tab, so it is intentionally NOT reset here.
        // The Main/Response panels now show a different tab's content; any
        // selection over the old one is stale.
        self.clear_selections();
        self.pending_autoscroll = None;
        // Keep focus where it is (usually the Tabs bar) so the user can move
        // across several tabs; only correct it if the current pane doesn't exist
        // in the newly-active tab's layout.
        if !self.panes().contains(&self.focus) {
            self.focus = Pane::Tabs;
        }
    }

    /// Closes the active tab (Ctrl+W / `x`) — index 0, the built-in Request
    /// tab, is never closable. A tab bound to a git-downloaded Workspace
    /// folder (see [`Collection::workspace_downloaded_from_git`]) is not
    /// closed immediately: since its folder was downloaded specifically for
    /// this tab (rather than being a folder the user already owned), the user
    /// is asked whether to keep it on disk (so the tab can still be reopened
    /// with `u`/Ctrl+Shift+T later) or delete it now — see
    /// [`Overlay::CloseGitWorkspace`] and [`Self::finish_close_tab`].
    pub(crate) fn close_active_tab(&mut self) {
        let idx = self.active_tab;
        if idx == 0 {
            return; // the built-in Request tab is not closable
        }
        if let Some(col) = self.collections.get(idx)
            && col.workspace_downloaded_from_git
            && let Some(path) = col.workspace_root.clone()
        {
            self.overlay = Some(Overlay::CloseGitWorkspace { idx, path, sel: 0 });
            return;
        }
        self.finish_close_tab(idx, false);
    }

    /// Actually removes tab `idx` from `self.collections`, either keeping it
    /// available for undo (`delete_folder == false`, the normal path for
    /// every ordinary tab) or, for a git-downloaded Workspace tab the user
    /// chose to delete, wiping its `workspace_root` folder from disk and
    /// skipping `closed_tabs` entirely — there would be nothing left on disk
    /// for `u`/Ctrl+Shift+T to reopen, so it must not be offered.
    pub(crate) fn finish_close_tab(&mut self, idx: usize, delete_folder: bool) {
        let removed = self.collections.remove(idx);
        if delete_folder {
            if let Some(root) = &removed.workspace_root {
                crate::git_remote::cleanup(root);
            }
        } else {
            // Remember it (with the index it was closed from) so Ctrl+Shift+T
            // can bring it back; capped so this can't grow unbounded in a
            // long session.
            self.closed_tabs.push((idx, removed));
            if self.closed_tabs.len() > 20 {
                self.closed_tabs.remove(0);
            }
        }
        self.active_tab = idx - 1;
        self.focus = Pane::Tabs;
        // Closing a real tab is reversible via `u` (reopen_closed_tab); flag
        // the undo path in the status bar since it's easy to close by accident.
        // A deleted git-workspace folder can't be reopened, so no hint there.
        if !delete_folder {
            self.status = Some(Status::TabClosed);
        }
        self.save_state();
    }

    /// Reopen the most recently closed tab (Ctrl+Shift+T), restoring it as
    /// close as possible to the index it was closed from and making it active.
    pub(crate) fn reopen_closed_tab(&mut self) {
        let Some((idx, col)) = self.closed_tabs.pop() else {
            return;
        };
        let idx = idx.min(self.collections.len());
        self.collections.insert(idx, col);
        self.active_tab = idx;
        self.focus = Pane::Tabs;
        self.save_state();
    }

    /// Move the active tab one position left/right among the reorderable tabs
    /// (index 0, the built-in Request tab, is fixed and never moved past).
    pub(crate) fn move_active_tab(&mut self, forward: bool) {
        let idx = self.active_tab;
        if idx == 0 {
            return;
        }
        let target = if forward { idx + 1 } else { idx - 1 };
        if target == 0 || target >= self.collections.len() {
            return;
        }
        self.collections.swap(idx, target);
        self.active_tab = target;
        self.save_state();
    }

    /// Begin moving (`is_move == true`) or copying (`is_move == false`) the
    /// currently highlighted request of the active tab into another collection
    /// file in the same workspace. Parks a clone of the request in
    /// [`TuiApp::pending_workspace_transfer`] and opens the workspace picker to
    /// choose a destination. A no-op unless the active tab is a Workspace tab
    /// with a request row (`WsRow::Request`) highlighted — so plain `m`/`c`
    /// never act on non-workspace tabs or folder/collection rows.
    pub(crate) fn start_workspace_transfer(&mut self, is_move: bool) {
        let ci = self.active_tab;
        let col = &self.collections[ci];
        if !col.is_workspace() || col.workspace_root.is_none() {
            return;
        }
        let Some(crate::collection::WsRow::Request(idx)) =
            col.ws_rows().into_iter().nth(col.list_cursor)
        else {
            return;
        };
        let Some(entry) = col.entries.get(idx).cloned() else {
            return;
        };
        self.pending_workspace_transfer = Some(PendingTransfer {
            entry,
            source_ci: ci,
            source_idx: idx,
            is_move,
        });
        self.open_workspace_transfer_picker(ci, is_move);
    }

    /// Delete the selected request from the active collection (works in any
    /// collection, including the Scratch Space). No-op when it is empty, or
    /// when the Requests list is currently browsing a folder/up row rather
    /// than highlighting an actual request (so `x` never deletes some
    /// unrelated, not-currently-visible entry). Remembers the removed entry
    /// (with the index it was removed from) so `u` can restore it — see
    /// [`Self::restore_deleted_request`].
    pub(crate) fn delete_selected_request(&mut self) {
        let ci = self.active_tab;
        let col = &self.collections[ci];
        if !matches!(
            col.rows().get(col.list_cursor),
            Some(crate::tree::Row::Entry(_))
        ) {
            return;
        }
        let col = &mut self.collections[ci];
        let idx = col.selected_entry.min(col.entries.len() - 1);
        let removed = col.entries.remove(idx);
        let method = removed.method.clone();
        col.deleted_entries.push((idx, removed));
        if col.deleted_entries.len() > 20 {
            col.deleted_entries.remove(0);
        }
        col.selected_entry = idx.min(col.entries.len().saturating_sub(1));
        col.sync_folder_to_selected();
        self.list_hscroll = 0;
        self.collections[ci].invalidate_request_json();
        self.status = Some(Status::RequestDeleted(method));
        self.save_state();
    }

    /// Reopen the most recently deleted request in the active collection
    /// (`u`, List pane), restoring it as close as possible to the index it
    /// was deleted from and selecting it. The exact parallel of
    /// [`Self::reopen_closed_tab`] for individual requests.
    pub(crate) fn restore_deleted_request(&mut self) {
        let ci = self.active_tab;
        let col = &mut self.collections[ci];
        let Some((idx, entry)) = col.deleted_entries.pop() else {
            return;
        };
        let idx = idx.min(col.entries.len());
        col.entries.insert(idx, entry);
        col.selected_entry = idx;
        col.sync_folder_to_selected();
        self.list_hscroll = 0;
        self.collections[ci].invalidate_request_json();
        self.status = None;
        self.save_state();
    }

    /// Ascend to the parent folder in the Requests list (Enter on the "up" row).
    fn list_folder_up(&mut self, ci: usize) {
        let col = &mut self.collections[ci];
        col.folder.pop();
        col.list_cursor = 0;
    }

    /// Descend into a subfolder in the Requests list (Enter on a folder row).
    fn list_folder_down(&mut self, ci: usize, name: String) {
        let col = &mut self.collections[ci];
        col.folder.push(name);
        col.list_cursor = 0;
    }

    /// Handle Enter on a Workspace tab's file-tree list row. `../` and
    /// subfolders navigate the filesystem breadcrumb; a collection file row
    /// opens/collapses it (switching files warns first if the current one has
    /// unsaved edits); a request row edits it.
    fn on_enter_workspace_list(&mut self, ci: usize) {
        let cursor = self.collections[ci].list_cursor;
        let Some(row) = self.collections[ci].ws_rows().into_iter().nth(cursor) else {
            self.focus = Pane::Main;
            return;
        };
        match row {
            crate::collection::WsRow::Up => {
                let col = &mut self.collections[ci];
                col.workspace_browse.pop();
                col.list_cursor = 0;
            }
            crate::collection::WsRow::Folder(name) => {
                self.workspace_folder_down(ci, name);
            }
            crate::collection::WsRow::Collection { path, open, .. } => {
                if open {
                    // The open collection's own row collapses it.
                    self.collections[ci].workspace_collapsed = true;
                } else {
                    self.activate_workspace_collection(ci, path);
                }
            }
            crate::collection::WsRow::Request(_) => {
                self.focus = Pane::Main;
                self.open_edit_request_wizard(ci);
            }
        }
    }

    /// Descend into a subfolder in a Workspace tab's file-tree list (Enter or
    /// Right on a folder row): push it onto the browse breadcrumb and reset
    /// the highlight to the top of the new folder.
    fn workspace_folder_down(&mut self, ci: usize, name: String) {
        let col = &mut self.collections[ci];
        col.workspace_browse.push(name);
        col.list_cursor = 0;
    }

    /// "Open" a *collapsed* collection file row in a Workspace tab (Enter or
    /// Right on it): re-expand it if it's the already-loaded file, otherwise
    /// load it (which warns first if the current file has unsaved edits, since
    /// loading replaces its entries wholesale).
    fn activate_workspace_collection(&mut self, ci: usize, path: PathBuf) {
        if self.collections[ci].path.as_deref() == Some(path.as_path()) {
            self.collections[ci].workspace_collapsed = false;
            self.collections[ci].sync_ws_cursor();
        } else {
            self.open_workspace_collection(ci, path);
        }
    }

    /// Load collection `path` into Workspace tab `ci`, first warning (via
    /// [`Overlay::WorkspaceSwitchUnsaved`]) if the currently-loaded file has
    /// unsaved in-memory edits that switching would discard.
    fn open_workspace_collection(&mut self, ci: usize, path: PathBuf) {
        if self.changed_request_count(ci) == 0 || self.collections[ci].path.is_none() {
            self.load_workspace_file(ci, path);
            return;
        }
        // Unsaved in-memory edits would be replaced by loading another file.
        // With "always save" on, auto-pick Save (switch only if the write
        // succeeded); otherwise ask.
        if self.always_save_when_prompted {
            if self.save_workspace_current_file(ci) {
                self.load_workspace_file(ci, path);
            }
            return;
        }
        self.overlay = Some(Overlay::WorkspaceSwitchUnsaved {
            ci,
            target: path,
            sel: 0,
        });
    }

    pub(crate) fn nav(&mut self, delta: i32) {
        let step = |cur: usize, len: usize, d: i32| -> usize {
            if len == 0 {
                return 0;
            }
            let ni = cur as i32 + d;
            ni.clamp(0, len as i32 - 1) as usize
        };
        let ci = self.active_tab;
        match self.focus {
            Pane::Tabs => {}
            Pane::List => {
                // Move within the current folder's rows, keeping
                // `selected_entry` pointed at whichever request is highlighted
                // so every other action (run, edit, delete, raw mode) keeps
                // acting on it. Workspace tabs use the filesystem file-tree
                // (`ws_rows`), ordinary tabs the title-folder tree (`rows`).
                let cur = self.collections[ci].list_cursor;
                if self.collections[ci].is_workspace() {
                    let rows = self.collections[ci].ws_rows();
                    let next = step(cur, rows.len(), delta);
                    self.collections[ci].list_cursor = next;
                    if let Some(crate::collection::WsRow::Request(idx)) = rows.get(next) {
                        self.collections[ci].selected_entry = *idx;
                    }
                } else {
                    let rows = self.collections[ci].rows();
                    let next = step(cur, rows.len(), delta);
                    self.collections[ci].list_cursor = next;
                    if let Some(crate::tree::Row::Entry(idx)) = rows.get(next) {
                        self.collections[ci].selected_entry = *idx;
                    }
                }
                self.main_scroll = 0;
                // Reset horizontal scroll so each newly selected name starts unscrolled.
                self.list_hscroll = 0;
                // The Main panel now shows a different entry's JSON; any
                // selection over the previous one is stale.
                self.clear_selections();
                self.pending_autoscroll = None;
            }
            Pane::GlobalEnv => {
                let len = self.global_envs.len();
                self.global_env_idx = step(self.global_env_idx, len, delta);
                // Reset horizontal scroll so each newly selected entry starts unscrolled.
                self.global_env_hscroll = 0;
            }
            Pane::Main => {
                let max = self.main_max_scroll as i32;
                self.main_scroll = (self.main_scroll as i32 + delta).clamp(0, max) as u16;
            }
            Pane::Response => {
                let max = self.resp_max_scroll as i32;
                self.resp_scroll = (self.resp_scroll as i32 + delta).clamp(0, max) as u16;
            }
        }
    }

    pub(crate) fn on_enter(&mut self) {
        let ci = self.active_tab;
        match self.focus {
            Pane::Response => {}
            // Enter on the tab bar renames the active tab (Left/Right already
            // switch the active tab while browsing, so whichever tab is
            // showing here is the one "selected"); the built-in Request tab
            // can't be renamed, so Enter there just moves focus into the list.
            Pane::Tabs => {
                if self.active_tab != 0 {
                    self.open_prompt_rename();
                } else {
                    self.focus = Pane::List;
                }
            }
            Pane::List => {
                // Enter's meaning depends on what's highlighted. Workspace
                // tabs use the filesystem file-tree (`ws_rows`); ordinary
                // tabs the title-folder tree (`rows`).
                if self.collections[ci].is_workspace() {
                    self.on_enter_workspace_list(ci);
                    return;
                }
                // A folder row descends into it, the "up" row ascends to the
                // parent folder, and a request row jumps straight into
                // editing it (same as pressing Enter again once focused on
                // the panel).
                match self.collections[ci]
                    .rows()
                    .get(self.collections[ci].list_cursor)
                {
                    Some(crate::tree::Row::Up) => self.list_folder_up(ci),
                    Some(crate::tree::Row::Folder(name)) => self.list_folder_down(ci, name.clone()),
                    Some(crate::tree::Row::Entry(_)) => {
                        self.focus = Pane::Main;
                        self.open_edit_request_wizard(ci);
                    }
                    // An empty list (no folders, no requests): still move
                    // focus into the panel, matching plain Enter elsewhere,
                    // but there's nothing to edit so no overlay opens.
                    None => self.focus = Pane::Main,
                }
            }
            Pane::GlobalEnv => {
                // Enter on a Global Environments list row opens a popup
                // showing that environment's variables (mirrors the old
                // inline panel's Enter-to-edit-secret behaviour, but scoped
                // to the popup rather than the collection-embedded panel).
                if let Some(env) = self.global_envs.get(self.global_env_idx) {
                    self.overlay = Some(Overlay::EnvPopup(EnvPopupState::new(env.id)));
                }
            }
            Pane::Main => {
                if !self.collections[ci].entries.is_empty() {
                    self.open_edit_request_wizard(ci);
                }
            }
        }
    }

    /// Open the "Edit Request" wizard overlay (the same form used for "New
    /// Request"), prefilled from the active tab's selected entry. This is the
    /// default action of Enter on a request; `open_raw_mode_editor` (Shift+H)
    /// is the secondary, Hurl-text-based editing path.
    fn open_edit_request_wizard(&mut self, ci: usize) {
        let s = Strings::for_language(&self.language);
        let names: Vec<String> = self
            .collections
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i == 0 {
                    s.tab_request.to_string()
                } else {
                    c.name.clone()
                }
            })
            .collect();
        let col = &self.collections[ci];
        let Some(entry) = col.entries.get(col.selected_entry) else {
            return;
        };
        let ei = col.selected_entry;
        let file_root = col
            .path
            .as_ref()
            .and_then(|p| p.parent().map(std::path::PathBuf::from));
        let form = NewReq::from_entry(ci, ei, entry, self.vars.base_url.clone(), names, file_root);
        self.overlay = Some(Overlay::NewRequest(Box::new(form)));
    }

    /// Open "Raw Mode": an editor showing the selected entry's actual Hurl-text
    /// representation, reparsed back into the entry on save (`PromptKind::Raw`).
    pub(crate) fn open_raw_mode_editor(&mut self, ci: usize) {
        let s = Strings::for_language(&self.language);
        let col = &self.collections[ci];
        let Some(entry) = col.entries.get(col.selected_entry) else {
            return;
        };
        let text = entry.to_hurl();
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::Raw(ci),
            editor: Editor::new(&text, true),
            title: s.entry_raw_hurl.to_string(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Open "Raw JSON Mode": an editor showing the selected entry's JSON
    /// preview (the same text [`build_request_json`] produces — and what the
    /// Main panel itself shows when `default_request_view` is JSON),
    /// reparsed back into the entry on save (`PromptKind::RawJson`). The
    /// JSON-text counterpart to Raw Mode (Shift+H).
    pub(crate) fn open_raw_json_editor(&mut self, ci: usize) {
        let s = Strings::for_language(&self.language);
        let col = &self.collections[ci];
        let Some(entry) = col.entries.get(col.selected_entry) else {
            return;
        };
        let text = build_request_json(entry);
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::RawJson(ci),
            editor: Editor::new(&text, true),
            title: s.entry_raw_json.to_string(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Append the request built in the New Request form to the active tab, or
    /// (when `form.editing` is set) apply the edits back onto the existing
    /// entry in place — preserving fields the wizard doesn't expose
    /// (`query_params`/`form_params`/`basic_auth`/`expected_status`).
    pub(crate) fn submit_new_request(&mut self, form: NewReq) {
        let url = form.url.text().trim().to_string();
        if url.is_empty() {
            return; // nothing to create/save
        }
        // An untitled request keeps an empty title so it lives at the root of
        // the folder tree (see `tree::entry_path`). Defaulting the title to
        // the URL used to be convenient, but a URL like `http://h/x` contains
        // slashes, so `entry_path` split it into phantom folders (`http:/…`),
        // filing the new request away in a nested section apart from its
        // siblings. The list already shows the URL for every row, so an empty
        // title loses nothing visually.
        let name = form.name.text().trim().to_string();
        let headers: Vec<(String, String)> = form
            .headers
            .iter()
            .filter(|r| r.enabled)
            .map(|r| {
                (
                    r.key.text().trim().to_string(),
                    r.value.text().trim().to_string(),
                )
            })
            .filter(|(k, _)| !k.is_empty())
            .collect();
        let cookies: Vec<(String, String)> = form
            .cookies
            .iter()
            .filter(|r| r.enabled)
            .map(|r| {
                (
                    r.key.text().trim().to_string(),
                    r.value.text().trim().to_string(),
                )
            })
            .filter(|(k, _)| !k.is_empty())
            .collect();
        let form_fields: Vec<FormField> = form
            .form_fields
            .iter()
            .filter(|r| r.enabled)
            .filter(|r| !r.key.text().trim().is_empty())
            .map(|r| {
                // For File-kind rows the Content-Type cell is the optional
                // Hurl content-type override; for Text rows it's ignored.
                // Desc is always UI-only and not persisted, matching Header
                // rows.
                let kind = r.kind;
                let content_type = if kind == FormFieldKind::File {
                    let ct = r.ctype.text().trim().to_string();
                    (!ct.is_empty()).then_some(ct)
                } else {
                    None
                };
                // The Base64 Prefix cell is only meaningful for Base64File
                // rows; store it verbatim (it may legitimately be empty).
                let base64_prefix =
                    (kind == FormFieldKind::Base64File).then(|| r.base64_prefix.text().to_string());
                FormField {
                    key: r.key.text().trim().to_string(),
                    value: r.value.text().trim().to_string(),
                    kind,
                    content_type,
                    base64_prefix,
                }
            })
            .collect();
        let asserts: Vec<String> = form
            .asserts
            .iter()
            .map(|r| r.expr.text().trim().to_string())
            .filter(|e| !e.is_empty())
            .collect();
        let captures: Vec<(String, String)> = form
            .captures
            .iter()
            .filter_map(|r| {
                let n = r.name.text().trim().to_string();
                let e = r.expr.text().trim().to_string();
                (!n.is_empty() && !e.is_empty()).then_some((n, e))
            })
            .collect();

        if let Some((ci, ei)) = form.editing {
            let Some(col) = self.collections.get_mut(ci) else {
                return;
            };
            let Some(entry) = col.entries.get_mut(ei) else {
                return;
            };
            let method = form.method().to_string();
            let body_text = form.body.text();
            let body = if body_text.trim().is_empty() {
                None
            } else {
                Some(body_text)
            };
            let changed = entry.title != name
                || entry.method != method
                || entry.url != url
                || entry.headers != headers
                || entry.cookies != cookies
                || entry.form_fields != form_fields
                || entry.body != body
                || entry.asserts != asserts
                || entry.captures != captures;
            if changed {
                entry.title = name;
                entry.method = method;
                entry.url = url;
                entry.headers = headers;
                entry.cookies = cookies;
                entry.form_fields = form_fields;
                entry.body = body;
                entry.asserts = asserts;
                entry.captures = captures;
                entry.modified = true;
            }
            col.invalidate_request_json();
            col.sync_folder_to_selected();
            self.active_tab = ci;
            self.focus = Pane::Main;
            self.status = None;
            self.save_state();
            return;
        }

        let mut entry =
            HurlEntry::from_fields(&name, form.method(), &url, headers, &form.body.text());
        entry.cookies = cookies;
        entry.form_fields = form_fields;
        entry.asserts = asserts;
        entry.captures = captures;
        let target = form.target_idx.min(self.collections.len() - 1);
        // Requests added to a real collection (not the Scratch Space, tab 0) are
        // marked so they're distinguishable from those loaded from the file.
        entry.user_added = target != 0;

        // A Workspace tab's entries belong to whichever file it currently has
        // loaded, so there's no single obvious destination — park the request
        // and let the user pick (or create) a collection in the workspace
        // tree instead of silently pushing it onto (or losing it against) the
        // loaded file.
        if self.collections[target].workspace_root.is_some() {
            self.pending_workspace_request = Some(entry);
            self.open_workspace_dest_picker(target);
            return;
        }

        self.active_tab = target;
        let col = &mut self.collections[target];
        col.entries.push(entry);
        col.selected_entry = col.entries.len() - 1;
        col.invalidate_request_json();
        col.sync_folder_to_selected();
        self.focus = Pane::Main;
        self.status = None;
        self.save_state();
    }

    /// Build a `TuiApp`, restoring saved state from the previous session.
    pub(crate) fn restored() -> Self {
        let mut app = Self::default();
        if let Some(state) = persistence::load_state() {
            app.apply_persisted(state);
        }
        app
    }

    /// Overwrite the current tabs / language / base URL from persisted state.
    /// `collections[0]` always remains the built-in Request tab.
    pub(crate) fn apply_persisted(&mut self, state: PersistedState) {
        self.language = state.language;
        if !state.base_url.trim().is_empty() {
            self.vars.base_url = state.base_url;
        }
        // Restore the global environment list first, gathering every
        // environment's pending secrets into one batch so restoring several
        // 1Password-backed environments only prompts for authorization once
        // in total, not once per environment.
        let mut pending_groups = Vec::new();
        let mut global_envs = Vec::with_capacity(state.global_envs.len());
        for pe in &state.global_envs {
            let (env, pending) = pe.restore();
            if !pending.is_empty() {
                pending_groups.push(PendingEnvSecrets {
                    env_id: env.id,
                    pending,
                });
            }
            global_envs.push(env);
        }
        self.active_env_id = state
            .active_global_env
            .and_then(|idx| global_envs.get(idx))
            .map(|e| e.id);
        self.global_envs = global_envs;
        if !pending_groups.is_empty() {
            self.pending_env.push(spawn_resolution_many(pending_groups));
        }
        if !state.tabs.is_empty() {
            // Track tabs whose Workspace root vanished entirely since the
            // last session (see `PersistedTab::into_collection`): a plain
            // status message for ones with nothing to redownload (a local
            // folder that was moved/deleted), or queued up for a
            // redownload-confirm prompt (see `open_next_pending_workspace_reload`)
            // for ones that are known to have come from git.
            let mut missing_workspace_name = None;
            let mut pending_reloads = std::collections::VecDeque::new();
            let collections = state
                .tabs
                .into_iter()
                .enumerate()
                .map(|(i, tab)| {
                    let had_root = tab.workspace_root.is_some();
                    let name = tab.name.clone();
                    let linked_env_id = tab
                        .linked_env_index
                        .and_then(|idx| self.global_envs.get(idx))
                        .map(|e| e.id);
                    let (col, pending_reload) = tab.into_collection(linked_env_id);
                    if had_root && col.workspace_root.is_none() {
                        match pending_reload {
                            Some(reload) => pending_reloads.push_back((i, reload)),
                            None => missing_workspace_name = Some(name),
                        }
                    }
                    col
                })
                .collect();
            self.collections = collections;
            if let Some(name) = missing_workspace_name {
                self.status = Some(Status::WorkspaceFolderMissing(name));
            }
            self.pending_workspace_reloads = pending_reloads;
            self.open_next_pending_workspace_reload();
        }
        self.active_tab = state.active_tab.min(self.collections.len() - 1);
        self.last_browse_dir = state
            .last_browse_dir
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        self.last_env_dir = state
            .last_env_dir
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        self.confirm_on_exit = state.confirm_on_exit;
        self.confirm_on_clear = state.confirm_on_clear;
        self.confirm_on_delete_env = state.confirm_on_delete_env;
        self.always_save_when_prompted = state.always_save_when_prompted;
        self.list_width = state.list_width;
        self.response_pct = state.response_pct;
        self.recent_git_urls = state.recent_git_urls;
        self.default_request_view = state.default_request_view;
        self.custom_themes = state.custom_themes;
        self.active_theme = state.active_theme;
    }

    /// Snapshot the current state for saving (environments are saved in source
    /// form only, so resolved secrets are never written to disk).
    pub(crate) fn to_persisted(&self) -> PersistedState {
        PersistedState {
            language: self.language.clone(),
            base_url: self.vars.base_url.clone(),
            tabs: self
                .collections
                .iter()
                .map(|c| {
                    let linked_env_index = c
                        .linked_env_id
                        .and_then(|id| self.global_envs.iter().position(|e| e.id == id));
                    PersistedTab::from_collection(c, linked_env_index)
                })
                .collect(),
            active_tab: self.active_tab,
            last_browse_dir: self
                .last_browse_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            last_env_dir: self
                .last_env_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            confirm_on_exit: self.confirm_on_exit,
            confirm_on_clear: self.confirm_on_clear,
            confirm_on_delete_env: self.confirm_on_delete_env,
            always_save_when_prompted: self.always_save_when_prompted,
            list_width: self.list_width,
            response_pct: self.response_pct,
            recent_git_urls: self.recent_git_urls.clone(),
            default_request_view: self.default_request_view,
            custom_themes: self.custom_themes.clone(),
            active_theme: self.active_theme.clone(),
            global_envs: self
                .global_envs
                .iter()
                .map(PersistedEnv::from_environment)
                .collect(),
            active_global_env: self
                .active_env_id
                .and_then(|id| self.global_envs.iter().position(|e| e.id == id)),
        }
    }

    pub(crate) fn save_state(&self) {
        persistence::save_state(&self.to_persisted());
    }

    /// Apply and persist the Default Request View submenu's highlighted
    /// index (0 = JSON, 1 = Hurl) — shared by every way of landing on a
    /// row (Up/Down/initial open), so hovering previews the setting live
    /// instead of only taking effect once the user presses Enter.
    fn apply_request_view(&mut self, sel: usize) {
        self.default_request_view = if sel == 0 {
            request::RequestView::Json
        } else {
            request::RequestView::Hurl
        };
        self.save_state();
    }

    /// Close all collections: drop every tab back to an empty Request tab and
    /// reset the base URL. Language and settings are kept and re-saved (so
    /// settings survive this action).
    pub(crate) fn clear_all(&mut self) {
        self.collections = vec![Collection::new("Request".to_string(), Vec::new())];
        self.active_tab = 0;
        self.vars.base_url = AppVars::default().base_url;
        self.focus = Pane::List;
        self.save_state();
    }

    /// Execute a confirmed action and dismiss the confirmation overlay.
    pub(crate) fn run_confirm(&mut self, action: ConfirmAction) {
        self.overlay = None;
        match action {
            ConfirmAction::Exit => self.quit = true,
            ConfirmAction::Clear => {
                self.clear_all();
                self.status = Some(Status::Cleared);
            }
            // Confirmed a change-overwrite of the ORIGINAL file: write there.
            ConfirmAction::Save(save) => {
                if let Some(path) = self.original_save_path(save) {
                    self.do_file_action(save, &path);
                }
            }
            // Confirmed a "Save As" over an existing file.
            ConfirmAction::Overwrite(save) => {
                if let Some(path) = self.pending_save_path.take() {
                    self.do_file_action(save, &path.to_string_lossy());
                }
            }
            ConfirmAction::DeleteEnv(idx) => self.delete_global_env(idx),
        }
    }

    /// The original file a collection / environment was loaded from (or last
    /// saved to), used by "Save …" to write back in place.
    fn original_save_path(&self, action: FileAction) -> Option<String> {
        let p = match action {
            FileAction::SaveCollection => self.collections.get(self.active_tab)?.path.as_ref(),
            FileAction::SaveEnv => {
                let env_id = self.current_env_id()?;
                self.global_envs
                    .iter()
                    .find(|e| e.id == env_id)?
                    .path
                    .as_ref()
            }
            _ => None,
        }?;
        Some(p.to_string_lossy().into_owned())
    }

    /// "Save Collection" / "Save Environment": write to the original file when
    /// one exists (silently if unchanged, else after a change confirmation);
    /// otherwise fall back to "Save As".
    pub(crate) fn begin_save(&mut self, action: FileAction) {
        if action == FileAction::SaveEnv && self.current_env_id().is_none() {
            self.status = Some(Status::NotEnvironment);
            return;
        }
        match self.original_save_path(action) {
            Some(path) => {
                let changes = match action {
                    FileAction::SaveEnv => self
                        .current_env_id()
                        .map(|id| self.changed_env_count(id))
                        .unwrap_or(0),
                    _ => self.changed_request_count(self.active_tab),
                };
                if changes == 0 {
                    // Nothing changed — saving to the original is a no-op; just do it.
                    self.do_file_action(action, &path);
                } else {
                    self.overlay = Some(Overlay::Confirm {
                        action: ConfirmAction::Save(action),
                        sel: 1,
                    });
                }
            }
            None => self.begin_save_as(action),
        }
    }

    /// "Save … As": prompt for a filename (with the extension ghost). On commit,
    /// an existing target triggers an overwrite confirmation.
    pub(crate) fn begin_save_as(&mut self, action: FileAction) {
        if action == FileAction::SaveEnv && self.current_env_id().is_none() {
            self.status = Some(Status::NotEnvironment);
            return;
        }
        let s = Strings::for_language(&self.language);
        let ci = self.active_tab;
        let (title, default) = match action {
            FileAction::SaveEnv => {
                let default = self.original_save_path(action).unwrap_or_else(|| {
                    let name = self
                        .current_env_id()
                        .and_then(|id| self.global_envs.iter().find(|e| e.id == id))
                        .map(|e| e.name.clone())
                        .unwrap_or_else(|| "environment".to_string());
                    format!("{name}.vars")
                });
                (s.save_environment, default)
            }
            _ => {
                let default = self
                    .original_save_path(action)
                    .unwrap_or_else(|| format!("{}.hurl", self.collections[ci].name));
                (s.save_collection, default)
            }
        };
        self.open_path_prompt(action, title, &default);
    }

    /// Save `action` to `path`, first asking to confirm an overwrite when the
    /// target already exists (collections/environments only).
    pub(crate) fn save_as_path(&mut self, action: FileAction, path: &str) {
        let exists = !path.is_empty() && std::path::Path::new(path).exists();
        if exists && matches!(action, FileAction::SaveCollection | FileAction::SaveEnv) {
            self.pending_save_path = Some(std::path::PathBuf::from(path));
            self.overlay = Some(Overlay::Confirm {
                action: ConfirmAction::Overwrite(action),
                sel: 1,
            });
        } else {
            self.do_file_action(action, path);
        }
    }

    /// Quit, first asking for confirmation when the setting is enabled — or when
    /// there are unsaved secret edits that would be lost (even if the setting is
    /// off), so the user is never silently robbed of secret changes.
    pub(crate) fn request_quit(&mut self) {
        if self.confirm_on_exit || self.has_unsaved_secret_changes() {
            self.overlay = Some(Overlay::Confirm {
                action: ConfirmAction::Exit,
                sel: 1,
            });
        } else {
            self.quit = true;
        }
    }

    pub(crate) fn open_prompt_baseurl(&mut self) {
        let s = Strings::for_language(&self.language);
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::BaseUrl,
            editor: Editor::new(&self.vars.base_url, false),
            title: s.base_url.to_string(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Open the two-field (`Key | Value`) form to add a variable by hand to
    /// the Global Environment `env_id` (from the entries popup's `n` key).
    pub(crate) fn open_prompt_add_env(&mut self, env_id: u64) {
        self.overlay = Some(Overlay::EnvVarForm(Box::new(EnvVarForm::new(env_id))));
    }

    /// Key handling for [`Overlay::EnvPopup`] — viewing/editing one Global
    /// Environment's variables (mirrors the shortcuts the old inline
    /// Environment panel had: n=add var, r=reload failed, Enter=edit
    /// secret/value, F2=rename the environment).
    fn on_key_env_popup(&mut self, mut popup: EnvPopupState, key: KeyEvent) {
        let len = self
            .global_envs
            .iter()
            .find(|e| e.id == popup.env_id)
            .map(|e| e.vars.len())
            .unwrap_or(0);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {}
            KeyCode::Up | KeyCode::Char('k') => {
                popup.idx = popup.idx.saturating_sub(1);
                popup.hscroll = 0;
                self.overlay = Some(Overlay::EnvPopup(popup));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if len > 0 {
                    popup.idx = (popup.idx + 1).min(len - 1);
                }
                popup.hscroll = 0;
                self.overlay = Some(Overlay::EnvPopup(popup));
            }
            KeyCode::Left | KeyCode::Char('h') => {
                let row_len = self
                    .global_envs
                    .iter()
                    .find(|e| e.id == popup.env_id)
                    .and_then(|e| e.vars.get(popup.idx))
                    .map(|v| v.key.chars().count() + 3 + v.display_value().chars().count())
                    .unwrap_or(0);
                popup.hscroll = clamp_hscroll(popup.hscroll, -4, row_len, popup.scroll_w.get());
                self.overlay = Some(Overlay::EnvPopup(popup));
            }
            KeyCode::Right | KeyCode::Char('l') => {
                let row_len = self
                    .global_envs
                    .iter()
                    .find(|e| e.id == popup.env_id)
                    .and_then(|e| e.vars.get(popup.idx))
                    .map(|v| v.key.chars().count() + 3 + v.display_value().chars().count())
                    .unwrap_or(0);
                popup.hscroll = clamp_hscroll(popup.hscroll, 4, row_len, popup.scroll_w.get());
                self.overlay = Some(Overlay::EnvPopup(popup));
            }
            KeyCode::Char('n') => self.open_prompt_add_env(popup.env_id),
            KeyCode::Char('r') => {
                self.overlay = Some(Overlay::EnvPopup(popup));
                self.reload_selected_env_var();
            }
            KeyCode::F(2) => {
                if self.global_envs.iter().any(|e| e.id == popup.env_id) {
                    self.open_prompt_rename_env(popup.env_id);
                } else {
                    self.overlay = Some(Overlay::EnvPopup(popup));
                }
            }
            KeyCode::Enter => {
                if let Some(var) = self
                    .global_envs
                    .iter()
                    .find(|e| e.id == popup.env_id)
                    .and_then(|e| e.vars.get(popup.idx))
                {
                    // Pre-fill the real value so it can be replaced, but mask its
                    // display for secrets so the value is never shown. Offer a
                    // reset back to the originally-loaded value.
                    let (val, title) = (var.value.clone(), var.key.clone());
                    let mask = var.is_secret();
                    let reset_to = Some(var.original_value.clone());
                    // A checkbox to declassify the value only makes sense for a
                    // variable sourced from a secret provider (1Password / SSM);
                    // it defaults to checked (still secret) — the safe choice.
                    let secret_checkbox = var.is_secret_source().then_some(true);
                    self.overlay = Some(Overlay::Prompt {
                        kind: PromptKind::EnvValue(popup.env_id, popup.idx),
                        editor: Editor::new(&val, false),
                        title,
                        mask,
                        reset_to,
                        secret_intact: mask,
                        secret_checkbox,
                    });
                } else {
                    self.overlay = Some(Overlay::EnvPopup(popup));
                }
            }
            _ => self.overlay = Some(Overlay::EnvPopup(popup)),
        }
    }

    /// Key handling for [`Overlay::EnvLinkPicker`] — linking/unlinking a
    /// Global Environment to a collection ('p' in the Requests list).
    fn on_key_env_link_picker(&mut self, mut picker: EnvLinkPicker, key: KeyEvent) {
        let total = self.global_envs.len() + 1; // +1 for "(none)"
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {}
            KeyCode::Up | KeyCode::Char('k') => {
                picker.sel = picker.sel.saturating_sub(1);
                self.overlay = Some(Overlay::EnvLinkPicker(picker));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.sel = (picker.sel + 1).min(total.saturating_sub(1));
                self.overlay = Some(Overlay::EnvLinkPicker(picker));
            }
            KeyCode::Enter => {
                let env_id = if picker.sel == 0 {
                    None
                } else {
                    self.global_envs.get(picker.sel - 1).map(|e| e.id)
                };
                self.set_linked_env(picker.ci, env_id);
            }
            _ => self.overlay = Some(Overlay::EnvLinkPicker(picker)),
        }
    }

    /// Key handling for [`Overlay::EnvCollision`] — resolving a name
    /// collision when loading an environment (Replace / Keep both / Abort /
    /// Rename then add).
    fn on_key_env_collision(&mut self, mut collision: EnvCollision, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {}
            KeyCode::Up | KeyCode::Char('k') => {
                collision.sel = collision.sel.saturating_sub(1);
                self.overlay = Some(Overlay::EnvCollision(Box::new(collision)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                collision.sel = (collision.sel + 1).min(3);
                self.overlay = Some(Overlay::EnvCollision(Box::new(collision)));
            }
            KeyCode::Enter => self.resolve_env_collision(collision),
            _ => self.overlay = Some(Overlay::EnvCollision(Box::new(collision))),
        }
    }

    /// Key handling for [`Overlay::WorkspacePicker`] — browsing a Workspace
    /// folder's recursive file tree to choose which collection file to load.
    /// Up/Down/j/k move across FILE rows only (directories are unselectable
    /// visual grouping); `Tab` toggles the `.hurl`/`.json` filter and
    /// re-scans; `Enter` loads the highlighted file into the target tab;
    /// Esc/q cancels, leaving the tab exactly as it was (possibly still with
    /// no collection chosen, if this was its first pick).
    fn on_key_workspace_picker(&mut self, mut picker: WorkspacePickerState, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // Cancelling without picking a file is non-destructive (the
                // tab stays around in its empty state so `w` can reopen this
                // later) — but stop auto-re-prompting every frame; only an
                // explicit `w` press (or switching away and back) should
                // bring the picker back now.
                if let Some(col) = self.collections.get_mut(picker.collection_idx)
                    && col.path.is_none()
                {
                    col.workspace_auto_prompt_dismissed = true;
                }
                // Abandon any parked new request or transfer so an aborted
                // "add/move/copy to workspace" flow doesn't leak state.
                self.pending_workspace_request = None;
                self.pending_workspace_transfer = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                picker.nav(-1);
                self.overlay = Some(Overlay::WorkspacePicker(picker));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.nav(1);
                self.overlay = Some(Overlay::WorkspacePicker(picker));
            }
            KeyCode::Tab => {
                picker.filter_hurl_json = !picker.filter_hurl_json;
                picker.rescan();
                if let Some(col) = self.collections.get_mut(picker.collection_idx) {
                    col.workspace_filter_hurl_json = picker.filter_hurl_json;
                }
                self.overlay = Some(Overlay::WorkspacePicker(picker));
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                // Create a brand-new collection in this workspace (the parked
                // request, if any, seeds it) instead of loading an existing
                // file — see `open_new_workspace_collection_prompt`.
                self.open_new_workspace_collection_prompt(picker.collection_idx);
            }
            KeyCode::Enter => match picker.entries.get(picker.selected) {
                Some(entry) if !entry.is_dir => {
                    let path = entry.path.clone();
                    let ci = picker.collection_idx;
                    match picker.mode {
                        WsPickerMode::MoveRequest | WsPickerMode::CopyRequest => {
                            // Transfer picks write straight to disk and don't
                            // switch the loaded file — the user stays put.
                            self.commit_workspace_transfer(path);
                        }
                        WsPickerMode::AddRequest => {
                            self.load_workspace_file(ci, path.clone());
                            // Only append if the load actually took effect; on
                            // a read error `load_workspace_file` leaves the tab
                            // untouched, so keep the request parked.
                            let loaded_ok =
                                self.collections.get(ci).and_then(|c| c.path.as_deref())
                                    == Some(path.as_path());
                            if loaded_ok {
                                self.append_pending_request_to_loaded(ci);
                            }
                        }
                        WsPickerMode::Browse => {
                            self.load_workspace_file(ci, path);
                        }
                    }
                }
                _ => self.overlay = Some(Overlay::WorkspacePicker(picker)),
            },
            _ => self.overlay = Some(Overlay::WorkspacePicker(picker)),
        }
    }

    pub(crate) fn open_prompt_rename(&mut self) {
        let ci = self.active_tab;
        if ci == 0 {
            return;
        }
        let s = Strings::for_language(&self.language);
        let name = self.collections[ci].name.clone();
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::RenameTab(ci),
            editor: Editor::new(&name, false),
            title: s.prompt_rename_title.to_string(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    /// Open the rename prompt for the Global Environment with `env_id`, if it
    /// still exists. Used both from the environments panel (F2) and the open
    /// environment-entries popup (F2).
    pub(crate) fn open_prompt_rename_env(&mut self, env_id: u64) {
        let Some(env) = self.global_envs.iter().find(|e| e.id == env_id) else {
            return;
        };
        let name = env.name.clone();
        let s = Strings::for_language(&self.language);
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::RenameEnv(env_id),
            editor: Editor::new(&name, false),
            title: s.env_rename_title.to_string(),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    pub(crate) fn activate_file_load_item(&mut self, sel: usize) {
        let s = Strings::for_language(&self.language);
        match sel {
            // Request is local-only, so it skips the source step entirely.
            0 => self.open_path_prompt(FileAction::LoadRequest, s.load_request, ""),
            1 => self.overlay = Some(Overlay::FileLoadSource(FileKind::Collection, 0)),
            2 => self.overlay = Some(Overlay::FileLoadSource(FileKind::Environment, 0)),
            _ => self.overlay = Some(Overlay::FileLoadSource(FileKind::Workspace, 0)),
        }
    }

    /// Second step of Load: `sel` is `0` for a local file, `1` for git.
    pub(crate) fn activate_file_load_source(&mut self, kind: FileKind, sel: usize) {
        let local = sel == 0;
        match (kind, local) {
            (FileKind::Collection, true) => self.open_browser(FileAction::OpenCollection),
            (FileKind::Collection, false) => self.open_remote_wizard(RemoteKind::Collection),
            (FileKind::Environment, true) => self.open_browser(FileAction::LoadEnv),
            (FileKind::Environment, false) => self.open_remote_wizard(RemoteKind::Environment),
            (FileKind::Workspace, true) => self.open_browser(FileAction::OpenWorkspace),
            (FileKind::Workspace, false) => self.open_remote_wizard(RemoteKind::Workspace),
        }
    }

    pub(crate) fn activate_file_save_item(&mut self, sel: usize) {
        let s = Strings::for_language(&self.language);
        match sel {
            // Request and Response are single-destination path prompts, so
            // they skip the destination step entirely.
            0 => self.open_path_prompt(FileAction::SaveRequest, s.save_request, "request.json"),
            1 => self.overlay = Some(Overlay::FileSaveDest(FileKind::Collection, 0)),
            2 => self.overlay = Some(Overlay::FileSaveDest(FileKind::Environment, 0)),
            3 => self.overlay = Some(Overlay::FileSaveDest(FileKind::Workspace, 0)),
            _ => self.open_path_prompt(FileAction::SaveResponse, s.save_response, "response.json"),
        }
    }

    /// Second step of Save: routes the destination `sel` for `kind`. The item
    /// list (and therefore `sel`) varies per kind — see
    /// [`crate::tui::app::file_save_dest_items`]:
    /// Collection = Save / Save As / To Git; Environment = Save / Save As;
    /// Workspace = Save As / To Git.
    pub(crate) fn activate_file_save_dest(&mut self, kind: FileKind, sel: usize) {
        match kind {
            // "Save …" writes back to the original file (confirming only when
            // there are changes); "Save … As" always prompts for a name.
            FileKind::Collection => match sel {
                0 => self.begin_save(FileAction::SaveCollection),
                1 => self.begin_save_as(FileAction::SaveCollection),
                _ => self.open_git_save_wizard(),
            },
            FileKind::Environment => match sel {
                0 => self.begin_save(FileAction::SaveEnv),
                _ => self.begin_save_as(FileAction::SaveEnv),
            },
            FileKind::Workspace => match sel {
                0 => self.begin_save_workspace_as(),
                _ => self.open_git_workspace_save_wizard(),
            },
        }
    }

    pub(crate) fn open_path_prompt(&mut self, action: FileAction, title: &str, default: &str) {
        let s = Strings::for_language(&self.language);
        self.overlay = Some(Overlay::Prompt {
            kind: PromptKind::FilePath(action),
            editor: Editor::new(default, false),
            title: format!("{}  ({})", title.trim_end_matches('…'), s.prompt_enter_path),
            mask: false,
            reset_to: None,
            secret_intact: false,
            secret_checkbox: None,
        });
    }

    pub(crate) fn open_browser(&mut self, action: FileAction) {
        let th = self.theme();
        let s = Strings::for_language(&self.language);
        let label = match action {
            FileAction::OpenCollection => s.open_collection,
            FileAction::LoadEnv => s.load_environment,
            FileAction::OpenWorkspace => s.open_workspace,
            FileAction::SaveWorkspaceChooseFolder => s.save_workspace,
            _ => s.browser_select_file,
        }
        .trim_end_matches('…');
        let hint_body = match action {
            FileAction::OpenWorkspace => s.browser_hint_workspace,
            FileAction::SaveWorkspaceChooseFolder => s.browser_hint_workspace_save,
            _ => s.browser_hint,
        };
        let hint = format!("{label}  ·  {hint_body}");

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(th.accent))
            .style(Style::default().bg(th.panel));
        let ex_theme = ExplorerTheme::default()
            .with_block(block)
            .add_default_title()
            .with_title_bottom(move |_| Line::from(hint.clone()))
            .with_item_style(Style::default().fg(th.text))
            .with_dir_style(Style::default().fg(th.accent).add_modifier(Modifier::BOLD))
            .with_highlight_item_style(
                Style::default()
                    .bg(th.accent)
                    .fg(th.bg)
                    .add_modifier(Modifier::BOLD),
            )
            .with_highlight_dir_style(
                Style::default()
                    .bg(th.accent)
                    .fg(th.bg)
                    .add_modifier(Modifier::BOLD),
            )
            .with_highlight_symbol("› ");

        match FileExplorerBuilder::build_with_theme(ex_theme) {
            Ok(mut ex) => {
                // Reopen in the last-used folder when it still exists. The
                // environment picker prefers the folder its own last file came
                // from, falling back to the shared last-browsed folder.
                let reopen = match action {
                    FileAction::LoadEnv => {
                        self.last_env_dir.as_ref().or(self.last_browse_dir.as_ref())
                    }
                    _ => self.last_browse_dir.as_ref(),
                };
                if let Some(dir) = reopen
                    && dir.is_dir()
                {
                    let _ = ex.set_cwd(dir);
                }
                // Remember where the browser actually started so `^r` can jump
                // back here after the user navigates away.
                self.browser_origin_dir = Some(ex.cwd().clone());
                self.browser_forward_path = None;
                self.overlay = Some(Overlay::Browser(action, Box::new(ex)));
            }
            Err(e) => self.status = Some(Status::Error(e.to_string())),
        }
    }

    fn help_key_handler(&mut self, key: KeyEvent, tab: usize) {
        match key.code {
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.overlay = Some(Overlay::Help(1 - tab));
                self.help_scroll = 0;
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.overlay = Some(Overlay::Help(1 - tab));
                self.help_scroll = 0;
            }
            // Scroll the body instead of closing the popup — on a small
            // terminal the Shortcuts/Glossary body can be taller than
            // the screen, and Up/Down used to just dismiss Help outright
            // (via the catch-all `_` arm below), making the rest of it
            // unreachable.
            KeyCode::Up => {
                self.overlay = Some(Overlay::Help(tab));
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                self.overlay = Some(Overlay::Help(tab));
                self.help_scroll = self.help_scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.overlay = Some(Overlay::Help(tab));
                self.help_scroll = self.help_scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                self.overlay = Some(Overlay::Help(tab));
                self.help_scroll = self.help_scroll.saturating_add(10);
            }
            KeyCode::Home => {
                self.overlay = Some(Overlay::Help(tab));
                self.help_scroll = 0;
            }
            KeyCode::End => {
                self.overlay = Some(Overlay::Help(tab));
                self.help_scroll = u16::MAX;
            }
            _ => {}
        }
    }

    fn close_git_workspace_key_handler(
        &mut self,
        key: KeyEvent,
        idx: usize,
        path: PathBuf,
        sel: usize,
    ) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Some(Overlay::CloseGitWorkspace {
                    idx,
                    path,
                    sel: (sel + 2) % 3,
                });
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.overlay = Some(Overlay::CloseGitWorkspace {
                    idx,
                    path,
                    sel: (sel + 1) % 3,
                });
            }
            KeyCode::Left => {
                self.overlay = Some(Overlay::CloseGitWorkspace {
                    idx,
                    path,
                    sel: (sel + 2) % 3,
                });
            }
            KeyCode::Right => {
                self.overlay = Some(Overlay::CloseGitWorkspace {
                    idx,
                    path,
                    sel: (sel + 1) % 3,
                });
            }
            KeyCode::Enter => match sel {
                0 => {
                    self.overlay = None;
                    self.finish_close_tab(idx, false);
                }
                1 => {
                    self.overlay = None;
                    self.finish_close_tab(idx, true);
                }
                _ => self.overlay = None,
            },
            _ => self.overlay = Some(Overlay::CloseGitWorkspace { idx, path, sel }),
        }
    }

    fn workspace_git_save_unsaved_key_handler(&mut self, key: KeyEvent, ci: usize, sel: usize) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Some(Overlay::WorkspaceGitSaveUnsaved {
                    ci,
                    sel: (sel + 2) % 3,
                });
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.overlay = Some(Overlay::WorkspaceGitSaveUnsaved {
                    ci,
                    sel: (sel + 1) % 3,
                });
            }
            KeyCode::Left => {
                self.overlay = Some(Overlay::WorkspaceGitSaveUnsaved {
                    ci,
                    sel: (sel + 2) % 3,
                });
            }
            KeyCode::Right => {
                self.overlay = Some(Overlay::WorkspaceGitSaveUnsaved {
                    ci,
                    sel: (sel + 1) % 3,
                });
            }
            KeyCode::Enter => match sel {
                // Save the in-memory edits to disk first, then push — but
                // only proceed if the save actually succeeded.
                0 => {
                    self.overlay = None;
                    if self.save_workspace_current_file(ci) {
                        self.start_git_workspace_save_wizard(ci);
                    }
                }
                // Push the on-disk version, leaving the edits in memory.
                1 => {
                    self.overlay = None;
                    self.start_git_workspace_save_wizard(ci);
                }
                _ => self.overlay = None,
            },
            _ => self.overlay = Some(Overlay::WorkspaceGitSaveUnsaved { ci, sel }),
        }
    }

    fn workspace_switch_unsaved_key_handler(
        &mut self,
        key: KeyEvent,
        ci: usize,
        target: PathBuf,
        sel: usize,
    ) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Left => {
                self.overlay = Some(Overlay::WorkspaceSwitchUnsaved {
                    ci,
                    target,
                    sel: (sel + 2) % 3,
                });
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Right => {
                self.overlay = Some(Overlay::WorkspaceSwitchUnsaved {
                    ci,
                    target,
                    sel: (sel + 1) % 3,
                });
            }
            KeyCode::Enter => match sel {
                // Save the in-memory edits to disk first, then switch —
                // but only if the save actually succeeded.
                0 => {
                    self.overlay = None;
                    if self.save_workspace_current_file(ci) {
                        self.load_workspace_file(ci, target);
                    }
                }
                // Discard the edits and switch.
                1 => {
                    self.overlay = None;
                    self.load_workspace_file(ci, target);
                }
                _ => self.overlay = None,
            },
            _ => self.overlay = Some(Overlay::WorkspaceSwitchUnsaved { ci, target, sel }),
        }
    }

    fn workspace_reload_confirm_key_handler(
        &mut self,
        key: KeyEvent,
        idx: usize,
        reload: Box<PendingWorkspaceReload>,
        sel: usize,
    ) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('n') | KeyCode::Char('N') => {
                // Declined: same outcome as any other Workspace whose
                // folder vanished — just explain why the tab looks
                // empty. The tab was already reset by `into_collection`.
                let name = reload.tab_name;
                self.overlay = None;
                self.status = Some(Status::WorkspaceFolderMissing(name));
                self.open_next_pending_workspace_reload();
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Char('l')
            | KeyCode::Char('k')
            | KeyCode::Char('j') => {
                self.overlay = Some(Overlay::WorkspaceReloadConfirm {
                    idx,
                    reload,
                    sel: 1 - sel,
                });
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.start_workspace_redownload(idx, *reload)
            }
            KeyCode::Enter => {
                if sel == 0 {
                    self.start_workspace_redownload(idx, *reload);
                } else {
                    let name = reload.tab_name;
                    self.overlay = None;
                    self.status = Some(Status::WorkspaceFolderMissing(name));
                    self.open_next_pending_workspace_reload();
                }
            }
            _ => self.overlay = Some(Overlay::WorkspaceReloadConfirm { idx, reload, sel }),
        }
    }

    fn workspace_storage_choice_key_handler(
        &mut self,
        key: KeyEvent,
        repo: PathBuf,
        name: String,
        origin: Option<WorkspaceGitOrigin>,
        sel: usize,
    ) {
        match key.code {
            KeyCode::Esc => {
                // Safe default: never lose the just-downloaded files —
                // fall back to keeping it temporary, same as sel == 0.
                self.confirm_workspace_root_from_git(repo, name, origin);
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Char('l')
            | KeyCode::Char('k')
            | KeyCode::Char('j') => {
                self.overlay = Some(Overlay::WorkspaceStorageChoice {
                    repo,
                    name,
                    origin,
                    sel: 1 - sel,
                });
            }
            KeyCode::Enter => {
                if sel == 0 {
                    self.confirm_workspace_root_from_git(repo, name, origin);
                } else {
                    self.pending_workspace_save = Some(PendingWorkspaceSave {
                        source_root: repo,
                        default_name: name,
                        target: WorkspaceSaveTarget::NewGitTab { origin },
                        dest_dir: None,
                    });
                    self.open_browser(FileAction::SaveWorkspaceChooseFolder);
                }
            }
            _ => {
                self.overlay = Some(Overlay::WorkspaceStorageChoice {
                    repo,
                    name,
                    origin,
                    sel,
                })
            }
        }
    }

    fn file_menu_key_handler(&mut self, key: KeyEvent, sel: usize) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {}
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Some(Overlay::FileMenu(sel.saturating_sub(1)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.overlay = Some(Overlay::FileMenu((sel + 1).min(1)));
            }
            KeyCode::Enter | KeyCode::Right => {
                self.overlay = Some(if sel == 0 {
                    Overlay::FileLoadMenu(0)
                } else {
                    Overlay::FileSaveMenu(0)
                });
            }
            KeyCode::Char(c) => {
                let s = Strings::for_language(&self.language);
                match mnemonic_index(&file_menu_items(&s), c) {
                    Some(0) => self.overlay = Some(Overlay::FileLoadMenu(0)),
                    Some(_) => self.overlay = Some(Overlay::FileSaveMenu(0)),
                    None => self.overlay = Some(Overlay::FileMenu(sel)),
                }
            }
            _ => self.overlay = Some(Overlay::FileMenu(sel)),
        }
    }

    fn file_load_menu_key_handler(&mut self, key: KeyEvent, sel: usize) {
        let s = Strings::for_language(&self.language);
        let items = file_load_items(&s);
        match key.code {
            // Left/Esc backs out to the top File menu; Right/Enter
            // descends into this kind's source (or activates it).
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left => {
                self.overlay = Some(Overlay::FileMenu(0))
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Some(Overlay::FileLoadMenu(sel.saturating_sub(1)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.overlay = Some(Overlay::FileLoadMenu((sel + 1).min(items.len() - 1)));
            }
            KeyCode::Enter | KeyCode::Right => self.activate_file_load_item(sel),
            KeyCode::Char(c) => match mnemonic_index(&items, c) {
                Some(i) => self.activate_file_load_item(i),
                None => self.overlay = Some(Overlay::FileLoadMenu(sel)),
            },
            _ => self.overlay = Some(Overlay::FileLoadMenu(sel)),
        }
    }

    fn file_save_menu_key_handler(&mut self, key: KeyEvent, sel: usize) {
        let s = Strings::for_language(&self.language);
        let items = file_save_items(&s);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left => {
                self.overlay = Some(Overlay::FileMenu(1))
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Some(Overlay::FileSaveMenu(sel.saturating_sub(1)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.overlay = Some(Overlay::FileSaveMenu((sel + 1).min(items.len() - 1)));
            }
            KeyCode::Enter | KeyCode::Right => self.activate_file_save_item(sel),
            KeyCode::Char(c) => match mnemonic_index(&items, c) {
                Some(i) => self.activate_file_save_item(i),
                None => self.overlay = Some(Overlay::FileSaveMenu(sel)),
            },
            _ => self.overlay = Some(Overlay::FileSaveMenu(sel)),
        }
    }

    fn file_load_source_key_handler(&mut self, key: KeyEvent, kind: FileKind, sel: usize) {
        let s = Strings::for_language(&self.language);
        let items = file_load_source_items(&s);
        match key.code {
            // Left/Esc steps back to the kind list with this kind lit.
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left => {
                self.overlay = Some(Overlay::FileLoadMenu(file_load_kind_index(kind)));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Some(Overlay::FileLoadSource(kind, sel.saturating_sub(1)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let n = items.len() - 1;
                self.overlay = Some(Overlay::FileLoadSource(kind, (sel + 1).min(n)));
            }
            KeyCode::Enter | KeyCode::Right => self.activate_file_load_source(kind, sel),
            KeyCode::Char(c) => match mnemonic_index(&items, c) {
                Some(i) => self.activate_file_load_source(kind, i),
                None => self.overlay = Some(Overlay::FileLoadSource(kind, sel)),
            },
            _ => self.overlay = Some(Overlay::FileLoadSource(kind, sel)),
        }
    }

    fn file_save_dest_key_handler(&mut self, key: KeyEvent, kind: FileKind, sel: usize) {
        let s = Strings::for_language(&self.language);
        let items = file_save_dest_items(kind, &s);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left => {
                self.overlay = Some(Overlay::FileSaveMenu(file_save_kind_index(kind)));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Some(Overlay::FileSaveDest(kind, sel.saturating_sub(1)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let n = items.len() - 1;
                self.overlay = Some(Overlay::FileSaveDest(kind, (sel + 1).min(n)));
            }
            KeyCode::Enter | KeyCode::Right => self.activate_file_save_dest(kind, sel),
            KeyCode::Char(c) => match mnemonic_index(&items, c) {
                Some(i) => self.activate_file_save_dest(kind, i),
                None => self.overlay = Some(Overlay::FileSaveDest(kind, sel)),
            },
            _ => self.overlay = Some(Overlay::FileSaveDest(kind, sel)),
        }
    }

    fn options_key_handler(&mut self, key: KeyEvent, sel: usize) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {}
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Some(Overlay::Options(sel.saturating_sub(1)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.overlay = Some(Overlay::Options((sel + 1).min(3)));
            }
            KeyCode::Enter => match sel {
                0 => {
                    // Open the Language submenu, preselecting the current language.
                    let cur = match self.language {
                        Language::English => 0,
                        Language::French => 1,
                        Language::Danish => 2,
                    };
                    self.overlay = Some(Overlay::LanguageMenu(cur));
                }
                1 => self.open_theme_editor(),
                2 => self.overlay = Some(Overlay::Preferences(0)),
                _ => {
                    // Close all collections, guarded by the confirm setting.
                    if self.confirm_on_clear {
                        self.overlay = Some(Overlay::Confirm {
                            action: ConfirmAction::Clear,
                            sel: 1,
                        });
                    } else {
                        self.clear_all();
                        self.status = Some(Status::Cleared);
                    }
                }
            },
            _ => self.overlay = Some(Overlay::Options(sel)),
        }
    }

    fn preferences_key_handler(&mut self, key: KeyEvent, sel: usize) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Some(Overlay::Options(2)),
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Some(Overlay::Preferences(sel.saturating_sub(1)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.overlay = Some(Overlay::Preferences((sel + 1).min(4)));
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                match sel {
                    0 => {
                        self.confirm_on_exit = !self.confirm_on_exit;
                        self.save_state();
                        self.overlay = Some(Overlay::Preferences(sel));
                    }
                    1 => {
                        self.confirm_on_clear = !self.confirm_on_clear;
                        self.save_state();
                        self.overlay = Some(Overlay::Preferences(sel));
                    }
                    2 => {
                        self.confirm_on_delete_env = !self.confirm_on_delete_env;
                        self.save_state();
                        self.overlay = Some(Overlay::Preferences(sel));
                    }
                    3 => {
                        self.always_save_when_prompted = !self.always_save_when_prompted;
                        self.save_state();
                        self.overlay = Some(Overlay::Preferences(sel));
                    }
                    _ => {
                        // Open the Default Request View submenu,
                        // preselecting the current view.
                        let cur = match self.default_request_view {
                            request::RequestView::Json => 0,
                            request::RequestView::Hurl => 1,
                        };
                        self.overlay = Some(Overlay::RequestViewMenu(cur));
                    }
                }
            }
            _ => self.overlay = Some(Overlay::Preferences(sel)),
        }
    }

    fn confirm_key_handler(&mut self, key: KeyEvent, action: ConfirmAction, sel: usize) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Char('n') | KeyCode::Char('N') => self.overlay = None,
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                // Two options (Yes = 0, No = 1); toggle between them.
                self.overlay = Some(Overlay::Confirm {
                    action,
                    sel: 1 - sel,
                });
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => self.run_confirm(action),
            KeyCode::Enter => {
                if sel == 0 {
                    self.run_confirm(action);
                } else {
                    self.overlay = None;
                }
            }
            _ => self.overlay = Some(Overlay::Confirm { action, sel }),
        }
    }

    fn language_menu_key_handler(&mut self, key: KeyEvent, sel: usize) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Some(Overlay::Options(0)),
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Some(Overlay::LanguageMenu(sel.saturating_sub(1)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.overlay = Some(Overlay::LanguageMenu((sel + 1).min(2)));
            }
            KeyCode::Enter => {
                self.language = match sel {
                    0 => Language::English,
                    1 => Language::French,
                    _ => Language::Danish,
                };
                self.save_state();
            }
            _ => self.overlay = Some(Overlay::LanguageMenu(sel)),
        }
    }

    fn request_view_menu_key_handler(&mut self, key: KeyEvent, sel: usize) {
        match key.code {
            // "Hovering" (moving the highlight with Up/Down) applies the
            // setting immediately, not just on Enter — so the user can
            // see how the Main panel actually renders each view while
            // still browsing the menu, the same live-preview feel as
            // arrowing over a colour swatch. Esc/Enter both just return
            // to Preferences afterwards; there's nothing left to
            // "confirm" or "cancel" since the value's already applied.
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                self.overlay = Some(Overlay::Preferences(4))
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let new_sel = sel.saturating_sub(1);
                Self::apply_request_view(self, new_sel);
                self.overlay = Some(Overlay::RequestViewMenu(new_sel));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let new_sel = (sel + 1).min(1);
                self.apply_request_view(new_sel);
                self.overlay = Some(Overlay::RequestViewMenu(new_sel));
            }
            _ => self.overlay = Some(Overlay::RequestViewMenu(sel)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn prompt_key_handler(
        &mut self,
        key: KeyEvent,
        kind: PromptKind,
        mut editor: Editor,
        title: String,
        mask: bool,
        reset_to: Option<String>,
        mut secret_intact: bool,
        mut secret_checkbox: Option<bool>,
    ) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        {
            enum Act {
                Commit,
                Cancel,
                Edit,
            }
            // A save prompt shows a `.hurl` / `.vars` ghost suffix; Tab, or
            // Right with the cursor at the end, autocompletes it.
            let ghost = kind.save_ghost();
            let at_end = editor.col >= editor.line_len(editor.row);
            let complete_ghost = !ghost.is_empty() && !editor.text().ends_with(ghost);
            let act = match key.code {
                KeyCode::Esc => Act::Cancel,
                KeyCode::F(2) => Act::Commit,
                // Ctrl+T toggles the "still secret?" checkbox (only present
                // when editing a secret-provider-sourced environment value).
                KeyCode::Char('t') if ctrl && secret_checkbox.is_some() => {
                    secret_checkbox = secret_checkbox.map(|v| !v);
                    Act::Edit
                }
                // Ctrl+R resets the field to its originally-loaded value (and,
                // for a secret, back to the fixed-width intact display).
                KeyCode::Char('r') if ctrl => {
                    if let Some(orig) = &reset_to {
                        let ml = editor.multiline;
                        editor = Editor::new(orig, ml);
                        secret_intact = mask;
                    }
                    Act::Edit
                }
                KeyCode::Enter => {
                    if editor.multiline && !ctrl {
                        editor.newline();
                        Act::Edit
                    } else {
                        Act::Commit
                    }
                }
                // Ctrl+Y copies the active in-editor selection (Shift+Arrow) to
                // the clipboard — `y` alone would just type the letter here, so
                // this mirrors the panel copy shortcut with a modifier that
                // can't collide with ordinary typing.
                KeyCode::Char('y') if ctrl => {
                    if let Some(text) = editor.selected_text() {
                        copy_to_clipboard(&text);
                        self.status = Some(Status::Copied);
                    }
                    Act::Edit
                }
                KeyCode::Char(c) => {
                    if secret_intact {
                        // Typing replaces the whole secret with fresh input.
                        editor = Editor::new("", editor.multiline);
                        secret_intact = false;
                    }
                    editor.clear_selection();
                    editor.insert(c);
                    Act::Edit
                }
                KeyCode::Backspace => {
                    if secret_intact {
                        // Clear the entire secret at once (never reveal its length).
                        editor = Editor::new("", editor.multiline);
                        secret_intact = false;
                    } else {
                        editor.clear_selection();
                        editor.backspace();
                    }
                    Act::Edit
                }
                // Cursor movement is meaningless while the intact secret is shown.
                // Shift+Arrow extends (or starts) a text selection instead of
                // just moving the cursor; a plain arrow move clears it.
                KeyCode::Left if !secret_intact => {
                    editor.set_selecting(shift);
                    editor.left();
                    Act::Edit
                }
                KeyCode::Tab if complete_ghost => {
                    editor.clear_selection();
                    editor.insert_str(ghost);
                    Act::Edit
                }
                KeyCode::Right if !secret_intact => {
                    editor.set_selecting(shift);
                    if complete_ghost && at_end {
                        editor.insert_str(ghost);
                    } else {
                        editor.right();
                    }
                    Act::Edit
                }
                KeyCode::Up if !secret_intact => {
                    editor.set_selecting(shift);
                    editor.up();
                    Act::Edit
                }
                KeyCode::Down if !secret_intact => {
                    editor.set_selecting(shift);
                    editor.down();
                    Act::Edit
                }
                KeyCode::Home if !secret_intact => {
                    editor.clear_selection();
                    editor.home();
                    Act::Edit
                }
                KeyCode::End if !secret_intact => {
                    editor.clear_selection();
                    editor.end();
                    Act::Edit
                }
                _ => Act::Edit,
            };
            match act {
                Act::Commit => {
                    // EnvValue/RenameEnv edits happen from within the
                    // entries popup — reopen it afterwards (matching the
                    // old inline panel, which never disappeared) so the
                    // user can keep working with the same environment.
                    let reopen_popup = match kind {
                        PromptKind::EnvValue(env_id, idx) => Some((env_id, idx)),
                        PromptKind::RenameEnv(env_id) => Some((env_id, 0)),
                        _ => None,
                    };
                    self.commit_prompt_with_secrecy(
                        kind,
                        editor.text(),
                        secret_checkbox.unwrap_or(true),
                    );
                    if let Some((env_id, idx)) = reopen_popup {
                        let mut popup = EnvPopupState::new(env_id);
                        popup.idx = idx;
                        self.overlay = Some(Overlay::EnvPopup(popup));
                    }
                }
                Act::Cancel => {
                    if matches!(kind, PromptKind::EnvValue(..) | PromptKind::RenameEnv(..)) {
                        let (env_id, idx) = match kind {
                            PromptKind::EnvValue(env_id, idx) => (env_id, idx),
                            PromptKind::RenameEnv(env_id) => (env_id, 0),
                            _ => unreachable!(),
                        };
                        let mut popup = EnvPopupState::new(env_id);
                        popup.idx = idx;
                        self.overlay = Some(Overlay::EnvPopup(popup));
                    } else if matches!(kind, PromptKind::WorkspaceSaveName) {
                        self.cancel_workspace_save();
                    } else if let PromptKind::NewWorkspaceCollection(ci) = kind {
                        // Cancelling the name prompt drops back to the
                        // workspace destination picker (still parked
                        // request, if any), rather than silently aborting
                        // the whole "add request" flow.
                        self.open_workspace_dest_picker(ci);
                    }
                }
                Act::Edit => {
                    self.overlay = Some(Overlay::Prompt {
                        kind,
                        editor,
                        title,
                        mask,
                        reset_to,
                        secret_intact,
                        secret_checkbox,
                    })
                }
            }
        }
    }

    fn env_var_form_key_handler(&mut self, key: KeyEvent, mut form: Box<EnvVarForm>) {
        match key.code {
            KeyCode::Esc => {
                self.overlay = Some(Overlay::EnvPopup(EnvPopupState::new(form.env_id)));
            }
            KeyCode::Enter => {
                let (k, v) = (form.key.text(), form.value.text());
                let env_id = form.env_id;
                self.add_env_var(env_id, k, v);
                // Return to the entries popup so the newly-added variable
                // is visible, selecting it.
                let mut popup = EnvPopupState::new(env_id);
                if let Some(env) = self.global_envs.iter().find(|e| e.id == env_id) {
                    popup.idx = env.vars.len().saturating_sub(1);
                }
                self.overlay = Some(Overlay::EnvPopup(popup));
            }
            // Tab / Shift+Tab move between the Key and Value cells.
            KeyCode::Tab | KeyCode::BackTab => {
                form.on_value = !form.on_value;
                self.overlay = Some(Overlay::EnvVarForm(form));
            }
            _ => {
                apply_edit_key(form.focused_mut(), key);
                self.overlay = Some(Overlay::EnvVarForm(form));
            }
        }
    }

    fn browser_key_handler(
        &mut self,
        key: KeyEvent,
        action: FileAction,
        mut ex: Box<FileExplorer>,
    ) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                // Cancelling the picker restores the parked wizard (if any)
                // unchanged, rather than discarding it.
                if let Some(form) = self.parked_wizard.take() {
                    self.overlay = Some(Overlay::NewRequest(form));
                } else if action == FileAction::SaveWorkspaceChooseFolder {
                    self.cancel_workspace_save();
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let on_parent_row = ex.cwd().parent() == Some(ex.current().path.as_path());
                if ex.current().is_dir && on_parent_row {
                    // The "../" row goes UP, which isn't a descent. Enter
                    // still honours it (the common file-picker idiom), but
                    // the arrow keys stay strictly directional: Right / l
                    // only ever go deeper, so a run of Rights can't bounce
                    // back up once a retrace lands on "../". Use Left (or
                    // Enter) to ascend.
                    if key.code == KeyCode::Enter {
                        self.browser_ascend(&mut ex);
                    }
                    self.overlay = Some(Overlay::Browser(action, ex));
                } else if ex.current().is_dir {
                    // Descend into the directory (handled by the explorer).
                    let target = ex.current().path.clone();
                    let _ = ex.handle(&Event::Key(key));
                    // If this descent retraces the upward trail, re-select
                    // the next folder down it so a run of Rights unwinds a
                    // run of Lefts exactly. Stepping into any other folder
                    // is a fresh navigation and clears the trail.
                    match self
                        .browser_forward_path
                        .as_ref()
                        .and_then(|trail| child_towards(&target, trail))
                    {
                        Some(next) => {
                            if let Some(idx) = ex.files().iter().position(|f| f.path == next) {
                                ex.set_selected_idx(idx);
                            }
                        }
                        None => self.browser_forward_path = None,
                    }
                    self.overlay = Some(Overlay::Browser(action, ex));
                } else if matches!(
                    action,
                    FileAction::OpenWorkspace | FileAction::SaveWorkspaceChooseFolder
                ) {
                    // A Workspace root/destination must be a folder, not
                    // a file — Enter on a file here is a no-op; `Space`
                    // picks the *current* folder instead (see below).
                    self.overlay = Some(Overlay::Browser(action, ex));
                } else {
                    // A file is selected — remember its folder so the browser
                    // reopens here next time, then perform the load.
                    self.last_browse_dir = Some(ex.cwd().clone());
                    if action == FileAction::LoadEnv {
                        // Environment picker tracks its own last folder.
                        self.last_env_dir = Some(ex.cwd().clone());
                    }
                    let path = ex.current().path.to_string_lossy().into_owned();
                    self.do_file_action(action, &path);
                    self.save_state();
                }
            }
            KeyCode::Char(' ') if action == FileAction::OpenWorkspace => {
                // Confirm the CURRENT WORKING DIRECTORY (not necessarily
                // the highlighted child) as the Workspace root — Enter is
                // reserved for descending further into subfolders.
                let root = ex.cwd().clone();
                self.last_browse_dir = Some(root.clone());
                self.confirm_workspace_root(root);
                self.save_state();
            }
            KeyCode::Char(' ') if action == FileAction::SaveWorkspaceChooseFolder => {
                // Confirm the CURRENT WORKING DIRECTORY as the
                // destination folder — the workspace's own (sub)folder
                // is created inside it once the following name prompt
                // is committed (see `workspace_save_pick_folder`).
                let dir = ex.cwd().clone();
                self.last_browse_dir = Some(dir.clone());
                self.workspace_save_pick_folder(dir);
            }
            KeyCode::Char('r') if ctrl => {
                // Snap back to the folder the browser first opened in —
                // handy after wandering far up/down the tree.
                if let Some(origin) = self.browser_origin_dir.clone()
                    && origin.is_dir()
                {
                    let _ = ex.set_cwd(&origin);
                }
                self.browser_forward_path = None;
                self.overlay = Some(Overlay::Browser(action, ex));
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') if !ctrl => {
                // Going up a level highlights the folder we just left
                // (rather than "../"), so an accidental Left is undone by
                // Right — instead of descending back into "../" and
                // climbing another level.
                self.browser_ascend(&mut ex);
                self.overlay = Some(Overlay::Browser(action, ex));
            }
            _ => {
                // Navigation (j/k, Home/End, Ctrl+h toggle hidden, …).
                let _ = ex.handle(&Event::Key(key));
                self.overlay = Some(Overlay::Browser(action, ex));
            }
        }
    }

    fn new_request_key_handler(&mut self, key: KeyEvent, mut form: Box<NewReq>) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let s = Strings::for_language(&self.language);
        let prev_focus = form.focus;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let submit = key.code == KeyCode::F(2) || (ctrl && key.code == KeyCode::Enter);
        // Arrowing onto an already-populated Key cell keeps the
        // dropdown hidden (see the focus-change handling below) so
        // Down/Up can move between rows instead of getting stuck
        // browsing suggestions. Enter explicitly re-reveals it.
        let reveal_key_dropdown =
            !submit && !ctrl && key.code == KeyCode::Enter && form.key_dropdown_revealable();
        if reveal_key_dropdown {
            form.suggest_hidden = false;
        }
        // Same reveal-on-Enter pattern for the Kind and Content-Type
        // dropdowns: a cell that already holds a value keeps its
        // dropdown hidden so Down/Up can move between rows, but
        // Enter explicitly reopens it for browsing.
        let reveal_kind_dropdown =
            !submit && !ctrl && key.code == KeyCode::Enter && form.kind_dropdown_revealable();
        if reveal_kind_dropdown {
            form.kind_dropdown_hidden = false;
        }
        let reveal_ctype_dropdown =
            !submit && !ctrl && key.code == KeyCode::Enter && form.ctype_dropdown_revealable();
        if reveal_ctype_dropdown {
            form.ctype_dropdown_hidden = false;
        }
        let dropdown = form.key_dropdown();
        let dropdown_open = dropdown.is_some();
        let sug_len = dropdown.as_ref().map(|(_, s)| s.len()).unwrap_or(0);
        let consume_up =
            !ctrl && dropdown_open && key.code == KeyCode::Up && form.suggest_hi.is_some();
        let consume_accept = dropdown_open
            && form.suggest_hi.is_some()
            && matches!(key.code, KeyCode::Enter | KeyCode::Tab);
        let kind_open = form.kind_dropdown_open();
        let ctype_open = form.ctype_dropdown_open();
        let ctype_len = form.ctype_option_count(&s); // +1 for "Auto", filtered by what's typed
        let mut keep = true;
        let mut do_submit = false;
        let mut typed_in_key = false;
        let mut typed_in_ctype = false;

        if key.code == KeyCode::Esc {
            if dropdown_open {
                // Dismiss the suggestion dropdown but keep the form open.
                form.suggest_hidden = true;
                form.suggest_hi = None;
            } else if kind_open {
                form.kind_dropdown_hidden = true;
            } else if ctype_open {
                form.ctype_dropdown_hidden = true;
                form.ctype_hi = None;
            } else {
                keep = false; // cancel the whole form
            }
        } else if submit {
            // A request can't be saved without a URL — that's the one
            // field `submit_new_request` bails on (silently discarding
            // everything else the user typed). Every other section
            // (headers/cookies/form fields/asserts/captures/body) is
            // either dropped-if-empty or stored as-is and only checked
            // at run time, so the URL is the sole save-time blocker.
            // Keep the wizard open, jump focus to the URL field, and
            // say why instead of closing on an empty URL.
            if form.url.text().trim().is_empty() {
                self.status = Some(Status::NewRequestUrlRequired);
                form.focus = NewField::Url;
                form.view_tab = WizardTab::All;
            } else {
                do_submit = true;
                keep = false;
            }
        } else if reveal_key_dropdown || reveal_kind_dropdown || reveal_ctype_dropdown {
            // The dropdown was just revealed above; stay put so the
            // user can browse it with Down/Up instead of also
            // advancing focus like a normal Enter would.
        } else if !ctrl && dropdown_open && key.code == KeyCode::Down {
            let last = sug_len - 1;
            form.suggest_hi = Some(match form.suggest_hi {
                None => 0,
                Some(k) => (k + 1).min(last),
            });
        } else if consume_up {
            form.suggest_hi = match form.suggest_hi {
                Some(0) | None => None,
                Some(k) => Some(k - 1),
            };
        } else if consume_accept {
            if let Some((_, sugs)) = dropdown.as_ref() {
                let k = form.suggest_hi.unwrap_or(0).min(sugs.len() - 1);
                let name = sugs[k];
                form.accept_suggestion(name);
            }
            form.focus_next(true);
        } else if !ctrl && kind_open && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            // Step through Text → File → Base64 File (Down) or the
            // reverse (Up), clamped at the ends like a small list.
            if let NewField::FormField(i, FormCol::Kind) = form.focus
                && let Some(row) = form.form_fields.get_mut(i)
            {
                row.kind = if key.code == KeyCode::Down {
                    match row.kind {
                        FormFieldKind::Text => FormFieldKind::File,
                        FormFieldKind::File => FormFieldKind::Base64File,
                        FormFieldKind::Base64File => FormFieldKind::Base64File,
                    }
                } else {
                    match row.kind {
                        FormFieldKind::Base64File => FormFieldKind::File,
                        FormFieldKind::File => FormFieldKind::Text,
                        FormFieldKind::Text => FormFieldKind::Text,
                    }
                };
            }
        } else if kind_open && key.code == KeyCode::Enter {
            // Enter just confirms the picked Type and closes the
            // dropdown — focus stays on the Kind cell since the
            // dropdown arrows can't accidentally steal it back.
            form.kind_dropdown_hidden = true;
        } else if kind_open && key.code == KeyCode::Tab {
            form.kind_dropdown_hidden = true;
            form.focus_next(true);
        } else if !ctrl && ctype_open && key.code == KeyCode::Down {
            let last = ctype_len - 1;
            form.ctype_hi = Some(match form.ctype_selected_index(&s) {
                None => 0,
                Some(k) => (k + 1).min(last),
            });
        } else if !ctrl && ctype_open && key.code == KeyCode::Up {
            form.ctype_hi = match form.ctype_selected_index(&s) {
                Some(0) | None => None,
                Some(k) => Some(k - 1),
            };
        } else if ctype_open
            && form.ctype_hi.is_some()
            && matches!(key.code, KeyCode::Enter | KeyCode::Tab)
        {
            form.accept_content_type(&s);
            form.focus_next(true);
        } else if !ctrl
            && key.code == KeyCode::Enter
            && let NewField::FormField(i, FormCol::Value) = form.focus
            && form
                .form_fields
                .get(i)
                .map(|r| r.kind)
                .is_some_and(|v| v.is_multipart())
        {
            // Enter on a `File`/`Base64File`-kind Form row's Value
            // cell opens the file picker too, not just Ctrl+F — it's
            // the more discoverable of the two.
            self.parked_wizard = Some(form);
            self.open_browser(FileAction::PickFormFile(i));
            return;
        } else if ctrl && key.code == KeyCode::Char('f') {
            // Ctrl+F opens a file picker for the focused Form row's
            // Value cell (only meaningful for `File`/`Base64File`
            // rows, which both point at a file).
            if let NewField::FormField(i, FormCol::Value) = form.focus
                && form
                    .form_fields
                    .get(i)
                    .map(|r| r.kind)
                    .is_some_and(|v| v.is_multipart())
            {
                self.parked_wizard = Some(form);
                self.open_browser(FileAction::PickFormFile(i));
                return;
            }
        } else if key.code == KeyCode::Tab {
            form.focus_next(true);
        } else if key.code == KeyCode::BackTab {
            form.focus_next(false);
        } else if key.code == KeyCode::PageDown {
            form.cycle_view_tab(true);
        } else if key.code == KeyCode::PageUp {
            form.cycle_view_tab(false);
        } else if !ctrl
            && !alt
            && matches!(key.code, KeyCode::Char('[') | KeyCode::Char(']'))
            && !form.focus_is_text_entry()
        {
            // `[` / `]` cycle the section-view tab too, mirroring the
            // main view's tab keys (an easier-to-reach alias for
            // PageUp/PageDown). Only active when focus isn't on a
            // text-entry cell, so the brackets can still be typed into
            // URLs, JSON bodies, header/cookie/form values, etc.
            form.cycle_view_tab(key.code == KeyCode::Char(']'));
        } else if ctrl && key.code == KeyCode::Char('d') {
            // Delete the focused Header/Cookie/Form/Assert/Capture row;
            // focus moves to the row sliding into its place, or the
            // section's "+ Add …" row once it's empty.
            form.delete_focused_row();
        } else if ctrl && key.code == KeyCode::Char('e') {
            // Toggle the focused row's enabled flag in place — focus
            // stays on whichever cell the user was editing instead
            // of jumping to the checkbox, so this can be pressed
            // mid-edit without derailing where they were typing.
            form.toggle_focused_enabled();
        } else if ctrl && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            // Ctrl+Arrow jumps straight to the next/previous section,
            // skipping the rest of the current section's rows/columns.
            form.focus = if key.code == KeyCode::Down {
                form.jump_forward()
            } else {
                form.jump_backward()
            };
        } else if alt && let KeyCode::Char(c @ '1'..='6') = key.code {
            // Alt+1..6 jumps directly to a section by number
            // (Headers/Cookies/Form/Body/Asserts/Captures), regardless
            // of the current section-view tab — a direct-jump
            // complement to Ctrl+Up/Down's sequential one. Alt (not
            // Ctrl) because Ctrl+<digit> has no standard control-code
            // encoding and most terminals only report it with a
            // modifier when the Kitty keyboard protocol is active;
            // Alt is sent as a plain ESC-prefix almost everywhere, so
            // it works without any special terminal support.
            let tab = match c {
                '1' => WizardTab::Headers,
                '2' => WizardTab::Cookies,
                '3' => WizardTab::Form,
                '4' => WizardTab::Body,
                '5' => WizardTab::Asserts,
                _ => WizardTab::Captures, // '6'
            };
            form.focus = form.first_field_of(tab);
        } else if ctrl && shift && matches!(key.code, KeyCode::Left | KeyCode::Right) {
            // Reorder the active section-view tab, mirroring how
            // collection tabs are reordered.
            form.move_view_tab(key.code == KeyCode::Right);
        } else {
            match form.focus {
                NewField::Method => match key.code {
                    KeyCode::Left | KeyCode::Char('h') => {
                        form.method_idx = (form.method_idx + METHODS.len() - 1) % METHODS.len();
                    }
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                        form.method_idx = (form.method_idx + 1) % METHODS.len();
                    }
                    KeyCode::Down => form.focus_next(true),
                    KeyCode::Up => form.focus_next(false),
                    _ => {}
                },
                NewField::Target => {
                    let n = form.target_names.len().max(1);
                    match key.code {
                        KeyCode::Left | KeyCode::Char('h') => {
                            form.target_idx = (form.target_idx + n - 1) % n;
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                            form.target_idx = (form.target_idx + 1) % n;
                        }
                        KeyCode::Down => form.focus_next(true),
                        KeyCode::Up => form.focus_next(false),
                        _ => {}
                    }
                }
                NewField::AddHeader => match key.code {
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        form.headers.push(HeaderRow::new());
                        form.focus = NewField::Header(form.headers.len() - 1, HdrCol::Key);
                    }
                    KeyCode::Down => form.focus_next(true),
                    KeyCode::Up => form.focus_next(false),
                    _ => {}
                },
                NewField::Header(i, col) => match key.code {
                    KeyCode::Up => {
                        // Move up a row, or leave the table upward to the
                        // URL section when already on the first row.
                        form.focus = if i > 0 {
                            NewField::Header(i - 1, col)
                        } else {
                            NewField::Url
                        };
                    }
                    KeyCode::Down => {
                        // Move down a row, or leave the table downward to
                        // the "Add header" section when on the last row.
                        form.focus = if i + 1 < form.headers.len() {
                            NewField::Header(i + 1, col)
                        } else {
                            NewField::AddHeader
                        };
                    }
                    KeyCode::Left => {
                        let at_start = form.headers[i]
                            .cell_mut(col)
                            .map(|ed| ed.col == 0)
                            .unwrap_or(true);
                        if !at_start {
                            if let Some(ed) = form.headers[i].cell_mut(col) {
                                if ctrl {
                                    ed.home()
                                } else {
                                    ed.left();
                                }
                            }
                        } else if let Some(prev) = form.prev_col(col) {
                            if let Some(ed) = form.headers[i].cell_mut(prev) {
                                ed.end();
                            }
                            form.focus = NewField::Header(i, prev);
                        }
                    }
                    KeyCode::Right => {
                        let at_end = form.headers[i]
                            .cell_mut(col)
                            .map(|ed| ed.col >= ed.line_len(ed.row))
                            .unwrap_or(true);
                        if !at_end {
                            if let Some(ed) = form.headers[i].cell_mut(col) {
                                if ctrl {
                                    ed.end();
                                } else {
                                    ed.right();
                                }
                            }
                        } else if let Some(next) = form.next_col(col) {
                            if let Some(ed) = form.headers[i].cell_mut(next) {
                                ed.home();
                            }
                            form.focus = NewField::Header(i, next);
                        }
                    }
                    KeyCode::Enter => form.focus_next(true),
                    KeyCode::Char(' ') if col == HdrCol::Enabled => {
                        if let Some(row) = form.headers.get_mut(i) {
                            row.enabled = !row.enabled;
                        }
                    }
                    _ => {
                        if let Some(ed) = form.headers[i].cell_mut(col) {
                            match key.code {
                                KeyCode::Char(ch) => ed.insert(ch),
                                KeyCode::Backspace => ed.backspace(),
                                KeyCode::Home => ed.home(),
                                KeyCode::End => ed.end(),
                                _ => {}
                            }
                        }
                    }
                },
                NewField::AddCookie => match key.code {
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        form.cookies.push(HeaderRow::new());
                        form.focus = NewField::Cookie(form.cookies.len() - 1, HdrCol::Key);
                    }
                    KeyCode::Down => form.focus_next(true),
                    KeyCode::Up => form.focus_next(false),
                    _ => {}
                },
                NewField::Cookie(i, col) => match key.code {
                    KeyCode::Up => {
                        form.focus = if i > 0 {
                            NewField::Cookie(i - 1, col)
                        } else {
                            form.up_into_headers()
                        };
                    }
                    KeyCode::Down => {
                        form.focus = if i + 1 < form.cookies.len() {
                            NewField::Cookie(i + 1, col)
                        } else {
                            NewField::AddCookie
                        };
                    }
                    KeyCode::Left => {
                        let at_start = form.cookies[i]
                            .cell_mut(col)
                            .map(|ed| ed.col == 0)
                            .unwrap_or(true);
                        if !at_start {
                            if let Some(ed) = form.cookies[i].cell_mut(col) {
                                if ctrl {
                                    ed.home();
                                } else {
                                    ed.left();
                                }
                            }
                        } else if let Some(prev) = form.prev_cookie_col(col) {
                            if let Some(ed) = form.cookies[i].cell_mut(prev) {
                                ed.end();
                            }
                            form.focus = NewField::Cookie(i, prev);
                        }
                    }
                    KeyCode::Right => {
                        let at_end = form.cookies[i]
                            .cell_mut(col)
                            .map(|ed| ed.col >= ed.line_len(ed.row))
                            .unwrap_or(true);
                        if !at_end {
                            if let Some(ed) = form.cookies[i].cell_mut(col) {
                                if ctrl {
                                    ed.end();
                                } else {
                                    ed.right();
                                }
                            }
                        } else if let Some(next) = form.next_cookie_col(col) {
                            if let Some(ed) = form.cookies[i].cell_mut(next) {
                                ed.home();
                            }
                            form.focus = NewField::Cookie(i, next);
                        }
                    }
                    KeyCode::Enter => form.focus_next(true),
                    KeyCode::Char(' ') if col == HdrCol::Enabled => {
                        if let Some(row) = form.cookies.get_mut(i) {
                            row.enabled = !row.enabled;
                        }
                    }
                    _ => {
                        if let Some(ed) = form.cookies[i].cell_mut(col) {
                            match key.code {
                                KeyCode::Char(ch) => ed.insert(ch),
                                KeyCode::Backspace => ed.backspace(),
                                KeyCode::Home => ed.home(),
                                KeyCode::End => ed.end(),
                                _ => {}
                            }
                        }
                    }
                },
                NewField::AddFormField => match key.code {
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        form.form_fields.push(FormRow::new());
                        form.focus = NewField::FormField(form.form_fields.len() - 1, FormCol::Key);
                    }
                    KeyCode::Down => form.focus_next(true),
                    KeyCode::Up => form.focus_next(false),
                    _ => {}
                },
                NewField::FormField(i, col) => match key.code {
                    KeyCode::Up => {
                        form.focus = if i > 0 {
                            NewField::FormField(i - 1, col)
                        } else {
                            form.up_into_cookies()
                        };
                    }
                    KeyCode::Down => {
                        form.focus = if i + 1 < form.form_fields.len() {
                            NewField::FormField(i + 1, col)
                        } else {
                            NewField::AddFormField
                        };
                    }
                    KeyCode::Left => {
                        let at_start = form.form_fields[i]
                            .cell_mut(col)
                            .map(|ed| ed.col == 0)
                            .unwrap_or(true);
                        if !at_start {
                            if let Some(ed) = form.form_fields[i].cell_mut(col) {
                                if ctrl {
                                    ed.home();
                                } else {
                                    ed.left();
                                }
                            }
                        } else if let Some(prev) = form.prev_form_col(col) {
                            if let Some(ed) = form.form_fields[i].cell_mut(prev) {
                                ed.end();
                            }
                            form.focus = NewField::FormField(i, prev);
                        }
                    }
                    KeyCode::Right => {
                        let at_end = form.form_fields[i]
                            .cell_mut(col)
                            .map(|ed| ed.col >= ed.line_len(ed.row))
                            .unwrap_or(true);
                        if !at_end {
                            if let Some(ed) = form.form_fields[i].cell_mut(col) {
                                if ctrl {
                                    ed.end();
                                } else {
                                    ed.right();
                                }
                            }
                        } else if let Some(next) = form.next_form_col(col) {
                            if let Some(ed) = form.form_fields[i].cell_mut(next) {
                                ed.home();
                            }
                            form.focus = NewField::FormField(i, next);
                        }
                    }
                    KeyCode::Enter => form.focus_next(true),
                    KeyCode::Char(' ') if col == FormCol::Enabled => {
                        if let Some(row) = form.form_fields.get_mut(i) {
                            row.enabled = !row.enabled;
                        }
                    }
                    _ => {
                        if let Some(ed) = form.form_fields[i].cell_mut(col) {
                            match key.code {
                                KeyCode::Char(ch) => ed.insert(ch),
                                KeyCode::Backspace => ed.backspace(),
                                KeyCode::Home => ed.home(),
                                KeyCode::End => ed.end(),
                                _ => {}
                            }
                        }
                    }
                },
                NewField::AddAssert => match key.code {
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        form.asserts.push(AssertRow::new());
                        form.focus = NewField::Assert(form.asserts.len() - 1);
                    }
                    KeyCode::Down => form.focus_next(true),
                    KeyCode::Up => form.focus_next(false),
                    _ => {}
                },
                NewField::Assert(i) => match key.code {
                    KeyCode::Up => {
                        form.focus = if i > 0 {
                            NewField::Assert(i - 1)
                        } else {
                            NewField::Body
                        };
                    }
                    KeyCode::Down => {
                        form.focus = if i + 1 < form.asserts.len() {
                            NewField::Assert(i + 1)
                        } else {
                            NewField::AddAssert
                        };
                    }
                    KeyCode::Enter => form.focus_next(true),
                    KeyCode::Char(ch) => {
                        if let Some(row) = form.asserts.get_mut(i) {
                            row.expr.insert(ch);
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(row) = form.asserts.get_mut(i) {
                            row.expr.backspace();
                        }
                    }
                    KeyCode::Left => {
                        if let Some(row) = form.asserts.get_mut(i) {
                            if ctrl {
                                row.expr.home();
                            } else {
                                row.expr.left();
                            }
                        }
                    }
                    KeyCode::Right => {
                        if let Some(row) = form.asserts.get_mut(i) {
                            if ctrl {
                                row.expr.end();
                            } else {
                                row.expr.right();
                            }
                        }
                    }
                    KeyCode::Home => {
                        if let Some(row) = form.asserts.get_mut(i) {
                            row.expr.home();
                        }
                    }
                    KeyCode::End => {
                        if let Some(row) = form.asserts.get_mut(i) {
                            row.expr.end();
                        }
                    }
                    _ => {}
                },
                NewField::AddCapture => match key.code {
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        form.captures.push(CaptureRow::new());
                        form.focus = NewField::Capture(form.captures.len() - 1, CapCol::Name);
                    }
                    KeyCode::Down => form.focus_next(true),
                    KeyCode::Up => form.focus_next(false),
                    _ => {}
                },
                NewField::Capture(i, col) => match key.code {
                    KeyCode::Up => {
                        form.focus = if i > 0 {
                            NewField::Capture(i - 1, col)
                        } else {
                            form.up_into_asserts()
                        };
                    }
                    KeyCode::Down => {
                        form.focus = if i + 1 < form.captures.len() {
                            NewField::Capture(i + 1, col)
                        } else {
                            NewField::AddCapture
                        };
                    }
                    KeyCode::Left => {
                        let at_start = form.captures[i].cell_mut(col).col == 0;
                        if !at_start {
                            if ctrl {
                                form.captures[i].cell_mut(col).home();
                            } else {
                                form.captures[i].cell_mut(col).left();
                            }
                        } else if let Some(prev) = form.prev_cap_col(col) {
                            form.captures[i].cell_mut(prev).end();
                            form.focus = NewField::Capture(i, prev);
                        }
                    }
                    KeyCode::Right => {
                        let ed = form.captures[i].cell_mut(col);
                        let at_end = ed.col >= ed.line_len(ed.row);
                        if !at_end {
                            if ctrl {
                                form.captures[i].cell_mut(col).end();
                            } else {
                                form.captures[i].cell_mut(col).right();
                            }
                        } else if let Some(next) = form.next_cap_col(col) {
                            form.captures[i].cell_mut(next).home();
                            form.focus = NewField::Capture(i, next);
                        }
                    }
                    KeyCode::Enter => form.focus_next(true),
                    KeyCode::Char(ch) => form.captures[i].cell_mut(col).insert(ch),
                    KeyCode::Backspace => form.captures[i].cell_mut(col).backspace(),
                    KeyCode::Home => form.captures[i].cell_mut(col).home(),
                    KeyCode::End => form.captures[i].cell_mut(col).end(),
                    _ => {}
                },
                _ => {
                    // Ghost Base URL: Right arrow on an empty URL field commits it.
                    if form.focus == NewField::Url
                        && key.code == KeyCode::Right
                        && form.url.text().is_empty()
                        && !form.base_url.is_empty()
                    {
                        form.url = Editor::new(&form.base_url, false);
                    } else if matches!(key.code, KeyCode::Up | KeyCode::Down) {
                        // Up/Down move between form sections. In the
                        // multiline Body they move the cursor within the
                        // text first, only leaving at the top/bottom edge.
                        let down = key.code == KeyCode::Down;
                        let leave = match form.active_editor() {
                            Some(ed) if ed.multiline => {
                                if down {
                                    if ed.row + 1 >= ed.lines.len() {
                                        true
                                    } else {
                                        ed.down();
                                        false
                                    }
                                } else if ed.row == 0 {
                                    true
                                } else {
                                    ed.up();
                                    false
                                }
                            }
                            _ => true, // single-line fields always move sections
                        };
                        if leave {
                            form.focus_next(down);
                        }
                    } else if let Some(ed) = form.active_editor() {
                        let single = !ed.multiline;
                        match key.code {
                            KeyCode::Enter if single => {}
                            KeyCode::Enter => ed.newline(),
                            KeyCode::Char(ch) => ed.insert(ch),
                            KeyCode::Backspace => ed.backspace(),
                            KeyCode::Left => ed.left(),
                            KeyCode::Right => ed.right(),
                            KeyCode::Home => ed.home(),
                            KeyCode::End => ed.end(),
                            _ => {}
                        }
                        if single && key.code == KeyCode::Enter {
                            form.focus_next(true);
                        }
                    }
                }
            }
            // Typing in the Key cell (re)opens the dropdown for the new text.
            if let NewField::Header(_, HdrCol::Key) = form.focus
                && matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace)
            {
                typed_in_key = true;
            }
            // Same for a File-kind row's Content-Type cell: typing
            // re-filters (and re-reveals, if a prior Enter/landing
            // had hidden it) the MIME-type dropdown for the new text.
            if let NewField::FormField(i, FormCol::Ctype) = form.focus
                && form.form_fields.get(i).map(|r| r.kind) == Some(FormFieldKind::File)
                && matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace)
            {
                typed_in_ctype = true;
            }
        }

        if do_submit {
            self.submit_new_request(*form);
        } else if keep {
            // Confine navigation to the active section tab: normal
            // dispatch above may have moved focus into a different
            // section (e.g. Tab off the last row, or Enter on a row's
            // last cell) which would be invisible while a single
            // section tab is showing. Snap back within the section
            // instead, wrapping to its first/last field depending on
            // which direction the key was moving.
            if form.view_tab != WizardTab::All
                && matches!(
                    key.code,
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down | KeyCode::Enter
                )
                && form.focus.wizard_section() != Some(form.view_tab)
            {
                let forward = matches!(key.code, KeyCode::Tab | KeyCode::Down | KeyCode::Enter);
                form.focus = if forward {
                    form.first_field_of(form.view_tab)
                } else {
                    form.view_tab.last_field()
                };
            }
            if typed_in_key {
                form.suggest_hi = None;
                form.suggest_hidden = false;
            } else if form.focus != prev_focus {
                // Moving to a different field resets the highlight, but
                // only auto-*shows* the dropdown when landing on an
                // empty Key cell (e.g. a freshly added header row).
                // Arrowing onto a Key cell that already has text must
                // not immediately trap Down/Up in the dropdown; Enter
                // can still reveal it explicitly (`reveal_key_dropdown`).
                form.suggest_hi = None;
                let landed_on_populated_key = matches!(form.focus, NewField::Header(i, HdrCol::Key)
                            if form.headers.get(i).is_some_and(|r| !r.key.text().is_empty()));
                form.suggest_hidden = landed_on_populated_key;
            }
            if form.focus != prev_focus {
                // Moving onto (or off of) the Kind cell resets its
                // dropdown, but it never auto-*shows*: a Kind cell
                // always has Text/File already picked (defaults to
                // Text on a fresh row), so landing on it always
                // leaves the dropdown hidden; Enter can still reveal
                // it explicitly (`reveal_kind_dropdown`).
                let landed_on_kind = matches!(form.focus, NewField::FormField(i, FormCol::Kind)
                            if form.form_fields.get(i).is_some());
                form.kind_dropdown_hidden = landed_on_kind;
            }
            if form.focus != prev_focus {
                // Moving onto (or off of) a File-kind Content-Type
                // cell resets its dropdown the same way: hidden if
                // the cell already has an override typed in, shown
                // for a fresh, empty cell.
                form.ctype_hi = None;
                let landed_on_populated_ctype = matches!(form.focus, NewField::FormField(i, FormCol::Ctype)
                            if form.form_fields.get(i).is_some_and(|r| !r.ctype.text().is_empty()));
                form.ctype_dropdown_hidden = landed_on_populated_ctype;
            } else if typed_in_ctype {
                // Typing on the same, already-focused Ctype cell:
                // reset the highlight and make sure the (now
                // re-filtered) dropdown is visible, even if it had
                // been hidden by a prior accept/landing.
                form.ctype_hi = None;
                form.ctype_dropdown_hidden = false;
            }
            self.overlay = Some(Overlay::NewRequest(form));
        }
    }

    /// Walk the file browser up one level, re-selecting the folder we just
    /// left (so an accidental step up is undone by stepping straight back in)
    /// and anchoring the retrace trail on the first step of an upward walk.
    /// No-op at the filesystem root, where there is nowhere further up to go.
    fn browser_ascend(&mut self, ex: &mut FileExplorer) {
        let here = ex.cwd().clone();
        if here.parent().is_some() {
            // The first step of a walk anchors the deepest folder as the trail
            // to retrace; later steps keep it (we're still climbing the chain).
            if self.browser_forward_path.is_none() {
                self.browser_forward_path = Some(here.clone());
            }
            let _ = ex.set_working_file(&here);
        }
    }
}

/// The direct child of `ancestor` that lies on the path down to `descendant`,
/// or `None` when `ancestor` is not a strict prefix of `descendant` (including
/// when they are equal). Used by the file browser to retrace an upward walk:
/// e.g. `child_towards("/a/b", "/a/b/c/d")` is `Some("/a/b/c")`.
fn child_towards(ancestor: &Path, descendant: &Path) -> Option<PathBuf> {
    let rest = descendant.strip_prefix(ancestor).ok()?;
    let first = rest.components().next()?;
    Some(ancestor.join(first))
}

/// Apply a horizontal-scroll `delta` to `current`, clamped to `[0, max]` where
/// `max` stops scrolling once the end of a `text_len`-char string is visible in
/// `content_w` columns (one column is reserved for the `‹` scrolled-off marker).
fn clamp_hscroll(current: u16, delta: i32, text_len: usize, content_w: u16) -> u16 {
    let max = text_len.saturating_sub((content_w as usize).saturating_sub(1)) as i32;
    (current as i32 + delta).clamp(0, max) as u16
}

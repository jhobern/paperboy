//! Internationalisation — language selection and all UI strings.
//!
//! All translations live in the `strings!` table below: one row per string,
//! listing its English / French / Danish text side by side. To add a language,
//! extend the macro and each row with the new column; to add a string, add one
//! row.

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum Language {
    #[default]
    English,
    French,
    Danish,
}

/// Every visible UI string, with its English / French / Danish text listed
/// side by side. Each row of the `strings!` table expands into a `Strings`
/// field and its value in each language; [`Strings::for_language`] returns the
/// set for one language. Adding a string is one row, so the translations can't
/// drift out of sync.
macro_rules! strings {
    ($($field:ident => $en:expr, $fr:expr, $da:expr;)*) => {
        // A terminal-only build still carries every string: the table is the
        // single place translations live, and splitting it by front-end is
        // exactly how the three languages would start to drift.
        #[cfg_attr(not(feature = "gui"), allow(dead_code))]
        pub struct Strings {
            $(pub $field: &'static str,)*
        }
        impl Strings {
            pub fn for_language(lang: &Language) -> Self {
                match lang {
                    Language::English => Self { $($field: $en,)* },
                    Language::French => Self { $($field: $fr,)* },
                    Language::Danish => Self { $($field: $da,)* },
                }
            }
        }
    };
}

strings! {
    app_heading => "🦀 PaperBoy", "🦀 PaperBoy", "🦀 PaperBoy";
    base_url => "Default New Request URL:", "URL par défaut des nouvelles requêtes\u{a0}:", "Standard-URL for nye anmodninger:";
    sending => "Sending…", "Envoi en cours…", "Sender…";
    response_heading => "Response", "Réponse", "Svar";
    status_label => "Status:", "Statut :", "Status:";
    response_time_label => "Time:", "Durée :", "Tid:";
    no_response_yet => "Run a request to see the response.", "Exécutez une requête pour voir la réponse.", "Kør en anmodning for at se svaret.";
    req_error_prefix => "Request error:", "Erreur de requête :", "Anmodningsfejl:";
    options_menu => "Settings", "Paramètres", "Indstillinger";
    options_menu_label => "(S)ettings", "Paramètre(s)", "Ind(s)tillinger";
    settings_item_language => "(L)anguage", "(L)angue", "(S)prog";
    settings_item_theme => "(T)heme", "(T)hème", "(T)ema";
    settings_item_preferences => "(P)references", "(P)références", "(P)ræferencer";
    settings_item_clear => "(C)lose all collections", "(F)ermer toutes les collections", "(L)uk alle samlinger";
    pref_item_confirm_exit => "Confirm on e(x)it", "Confirmer à la (s)ortie", "Bekræft ved (a)fslutning";
    pref_item_confirm_clear => "Confirm on (c)lear", "Confirmer avant de (f)ermer", "Bekræft ved (l)ukning";
    pref_item_confirm_delete_env => "Confirm before (d)eleting an environment", "Confirmer avant de supprimer un (e)nvironnement", "Bekræft før (s)letning af et miljø";
    pref_item_always_save => "(A)lways save unsaved changes when prompted", "(T)oujours enregistrer les modifications non enregistrées lorsque demandé", "(G)em altid ugemte ændringer, når du bliver spurgt";
    pref_item_default_view => "Default Request (V)iew", "(V)ue de requête par défaut", "Standard anmodnings(v)isning";
    pref_item_run_all_batch => "Run All in (b)atch mode (chains cookies & captures)", "Tout exécuter en mode (b)atch (chaîne cookies et captures)", "Kør alle i (b)atch-tilstand (kæder cookies og optagelser)";
    language_label => "Language", "Langue", "Sprog";
    lang_english => "English", "English", "English";
    lang_french => "Français", "Français", "Français";
    lang_danish => "Dansk", "Dansk", "Dansk";
    theme_editor_title => "Theme", "Thème", "Tema";
    theme_auto => "Automatic (match language)", "Automatique (selon la langue)", "Automatisk (efter sprog)";
    theme_name_label => "Name", "Nom", "Navn";
    theme_editor_hint => "^N new · ^D delete · Esc close", "^N nouveau · ^D supprimer · Échap fermer", "^N nyt · ^D slet · Esc luk";
    theme_fields_hint => "Enter edit colour · Tab list · Esc close", "Entrée modifier la couleur · Tab liste · Échap fermer", "Enter rediger farve · Tab liste · Esc luk";
    theme_c_bg => "Background", "Arrière-plan", "Baggrund";
    theme_c_panel => "Panel", "Panneau", "Panel";
    theme_c_text => "Text", "Texte", "Tekst";
    theme_c_dim => "Dim text", "Texte atténué", "Dæmpet tekst";
    theme_c_accent => "Accent", "Accent", "Accent";
    theme_c_ok => "Success", "Succès", "Succes";
    theme_c_err => "Error", "Erreur", "Fejl";
    theme_c_subst => "Substitution", "Substitution", "Substitution";
    theme_c_pending => "Pending", "En attente", "Afventer";
    theme_c_select_bg => "Selection bg", "Sélection fond", "Markering bg";
    theme_c_select_fg => "Selection text", "Sélection texte", "Markering tekst";
    theme_saved => "Theme saved:", "Thème enregistré\u{a0}:", "Tema gemt:";
    theme_deleted => "Theme deleted:", "Thème supprimé\u{a0}:", "Tema slettet:";
    theme_name_required => "Give the theme a name before saving", "Donnez un nom au thème avant d'enregistrer", "Giv temaet et navn før du gemmer";
    theme_name_reserved => "That name belongs to a built-in preset; choose another", "Ce nom appartient à un préréglage intégré\u{a0}; choisissez-en un autre", "Det navn tilhører en indbygget forudindstilling; vælg et andet";
    theme_cannot_delete => "Built-in presets can't be deleted", "Les préréglages intégrés ne peuvent pas être supprimés", "Indbyggede forudindstillinger kan ikke slettes";
    theme_name_taken => "A theme with that name already exists", "Un thème portant ce nom existe déjà", "Der findes allerede et tema med det navn";
    theme_preset_readonly => "Presets can't be edited — press ^N to make a copy", "Les préréglages ne peuvent pas être modifiés — appuyez sur ^N pour en faire une copie", "Forudindstillinger kan ikke redigeres — tryk på ^N for at lave en kopi";
    theme_new_title => "New theme", "Nouveau thème", "Nyt tema";
    theme_new_base => "Base on", "Basé sur", "Baseret på";
    theme_new_popup_hint => "Enter create · Esc cancel", "Entrée créer · Échap annuler", "Enter opret · Esc annuller";
    theme_ch_red => "R", "R", "R";
    theme_ch_green => "G", "G", "G";
    theme_ch_blue => "B", "B", "B";
    theme_color_popup_hint => "←/→ ±1 · ^←/^→ or PgUp/Dn ±16 · type 0-255 · ↑/↓ channel · Enter apply · Esc cancel", "←/→ ±1 · ^←/^→ ou PgPréc/Suiv ±16 · saisir 0-255 · ↑/↓ canal · Entrée appliquer · Échap annuler", "←/→ ±1 · ^←/^→ eller PgUp/Ned ±16 · indtast 0-255 · ↑/↓ kanal · Enter anvend · Esc annuller";
    clear_all_done => "All collections closed", "Toutes les collections ont été fermées", "Alle samlinger er lukket";
    copied_to_clipboard => "Copied to clipboard", "Copié dans le presse-papiers", "Kopieret til udklipsholder";
    preferences_menu => "Preferences", "Préférences", "Præferencer";
    confirm_exit_secrets => "There are environment secrets with unsaved changes, exiting will cause these changes to be lost.", "Il y a des secrets d'environnement avec des modifications non enregistrées, quitter entraînera la perte de ces modifications.", "Der er miljøhemmeligheder med ikke-gemte ændringer. Hvis du afslutter, vil disse ændringer gå tabt.";
    confirm_exit_edits => "{n} request(s) have edits that have not been saved; exiting will cause these changes to be lost.", "{n} requête(s) ont des modifications non enregistrées\u{a0}; quitter entraînera la perte de ces modifications.", "{n} anmodning(er) har ændringer, der ikke er gemt; hvis du afslutter, vil disse ændringer gå tabt.";
    gui_unsaved_quit_title => "Unsaved changes", "Modifications non enregistrées", "Ikke-gemte ændringer";
    gui_unsaved_quit_q => "{n} request(s) in {t} have edits that have not been saved. Quitting now will lose them.", "{n} requête(s) dans {t} ont des modifications non enregistrées. Quitter maintenant les perdra.", "{n} anmodning(er) i {t} har ændringer, der ikke er gemt. Hvis du afslutter nu, går de tabt.";
    gui_unsaved_close_tab_q => "{n} request(s) in \"{t}\" have edits that have not been saved. Closing this tab will lose them.", "{n} requête(s) dans «\u{a0}{t}\u{a0}» ont des modifications non enregistrées. Fermer cet onglet les perdra.", "{n} anmodning(er) i \"{t}\" har ændringer, der ikke er gemt. Hvis du lukker denne fane, går de tabt.";
    gui_quit_anyway => "Quit anyway", "Quitter quand même", "Afslut alligevel";
    gui_save_all_and_quit => "Save all changes", "Tout enregistrer", "Gem alle ændringer";
    gui_saved_n_files => "Saved {n} file(s)", "{n} fichier(s) enregistré(s)", "Gemte {n} fil(er)";
    gui_close_anyway => "Close anyway", "Fermer quand même", "Luk alligevel";
    default_request_view_label => "Default Request View", "Vue de requête par défaut", "Standard anmodningsvisning";
    view_json_label => "JSON", "JSON", "JSON";
    view_hurl_label => "Hurl", "Hurl", "Hurl";
    confirm_exit_q => "Quit PaperBoy?", "Quitter PaperBoy\u{a0}?", "Afslut PaperBoy?";
    confirm_clear_q => "Close all collections? This removes all tabs and requests.", "Fermer toutes les collections\u{a0}? Cela supprime tous les onglets et requêtes.", "Luk alle samlinger? Dette fjerner alle faner og anmodninger.";
    confirm_save_collection_q => "There are {r} new or modified request entries. Saving will overwrite the original collection file. Proceed?", "Il y a {r} requête(s) nouvelle(s) ou modifiée(s). L'enregistrement écrasera le fichier de collection d'origine. Continuer\u{a0}?", "Der er {r} nye eller ændrede anmodninger. Gemning vil overskrive den oprindelige samlingsfil. Fortsæt?";
    confirm_save_env_q => "There are {e} new or modified environment entries. Saving will overwrite the original environment file. Proceed?", "Il y a {e} variable(s) d'environnement nouvelle(s) ou modifiée(s). L'enregistrement écrasera le fichier d'environnement d'origine. Continuer\u{a0}?", "Der er {e} nye eller ændrede miljøvariabler. Gemning vil overskrive den oprindelige miljøfil. Fortsæt?";
    confirm_save_report_q => "This report has unsaved edits. Saving will overwrite the original report file. Proceed?", "Ce rapport a des modifications non enregistrées. L'enregistrement écrasera le fichier de rapport d'origine. Continuer\u{a0}?", "Denne rapport har ugemte ændringer. Gemning vil overskrive den oprindelige rapportfil. Fortsæt?";
    confirm_overwrite_q => "\"{f}\" already exists. Overwrite it?", "«\u{a0}{f}\u{a0}» existe déjà. L'écraser\u{a0}?", "«{f}» findes allerede. Overskriv den?";
    confirm_revert_request_q => "Revert \"{r}\" to its last saved version? In-memory edits will be discarded.", "Rétablir «\u{a0}{r}\u{a0}» à sa dernière version enregistrée\u{a0}? Les modifications en mémoire seront perdues.", "Gendan «{r}» til sidst gemte version? Ændringer i hukommelsen går tabt.";
    confirm_revert_file_q => "Revert \"{f}\" to its last saved version? Every in-memory edit to it will be discarded.", "Rétablir «\u{a0}{f}\u{a0}» à sa dernière version enregistrée\u{a0}? Toutes les modifications en mémoire seront perdues.", "Gendan «{f}» til sidst gemte version? Alle ændringer i hukommelsen går tabt.";
    confirm_revert_env_q => "Revert {n} change(s) in \"{e}\" to the last saved values?", "Rétablir {n} modification(s) dans «\u{a0}{e}\u{a0}» aux dernières valeurs enregistrées\u{a0}?", "Gendan {n} ændring(er) i «{e}» til de sidst gemte værdier?";
    confirm_rerun_report_q => "This will replace the current results, which you haven't exported. Rerun anyway?", "Cela remplacera les résultats actuels, que vous n'avez pas exportés. Relancer quand même\u{a0}?", "Dette erstatter de nuværende resultater, som du ikke har eksporteret. Kør igen alligevel?";
    confirm_yes => "Yes", "Oui", "Ja";
    confirm_no => "No", "Non", "Nej";
    file_menu => "File", "Fichier", "Fil";
    file_menu_label => "(F)ile", "(F)ichier", "(F)il";
    file_menu_item_load => "(L)oad", "(C)harger", "(I)ndlæs";
    file_menu_item_save => "(S)ave", "(E)nregistrer", "(G)em";
    file_load_menu => "Load", "Charger", "Indlæs";
    file_save_menu => "Save", "Enregistrer", "Gem";
    file_kind_collection => "Collection", "Collection", "Samling";
    file_kind_environment => "Environment", "Environnement", "Miljø";
    file_kind_workspace => "Workspace", "Workspace", "Workspace";
    file_kind_report => "Report", "Rapport", "Rapport";
    file_source_local => "(L)ocal file…", "Fichier (l)ocal…", "(L)okal fil…";
    file_source_git => "From (G)it…", "Depuis (G)it…", "Fra (G)it…";
    file_source_postman => "From (P)ostman…", "Depuis (P)ostman…", "Fra (P)ostman…";
    file_dest_save => "(S)ave", "(E)nregistrer", "(G)em";
    file_dest_save_as => "Save (A)s…", "Enregistrer s(o)us…", "Gem s(o)m…";
    file_dest_git => "To (G)it…", "Vers (G)it…", "Til Gi(t)…";
    file_load_item_request => "(R)equest…", "(R)equête…", "(A)nmodning…";
    file_load_item_collection => "(C)ollection…", "(C)ollection…", "(S)amling…";
    file_load_item_environment => "(E)nvironment…", "(E)nvironnement…", "(M)iljø…";
    file_load_item_workspace => "(W)orkspace…", "(W)orkspace…", "(W)orkspace…";
    file_load_item_report => "Repor(t)…", "Rappor(t)…", "Rappor(t)…";
    file_save_item_request => "(R)equest…", "(R)equête…", "(A)nmodning…";
    file_save_item_collection => "(C)ollection…", "(C)ollection…", "(S)amling…";
    file_save_item_environment => "(E)nvironment…", "(E)nvironnement…", "(M)iljø…";
    file_save_item_workspace => "(W)orkspace…", "(W)orkspace…", "(W)orkspace…";
    file_save_item_report => "Repor(t)…", "Rappor(t)…", "Rappor(t)…";
    file_save_item_response => "Res(p)onse…", "Ré(p)onse…", "S(v)ar…";
    save_request => "Save Request…", "Enregistrer la requête…", "Gem anmodning…";
    load_request => "Load Request…", "Charger une requête…", "Indlæs anmodning…";
    open_collection => "Load Collection…", "Charger une collection…", "Indlæs samling…";
    open_report => "Load Report…", "Charger un rapport…", "Indlæs rapport…";
    save_report_folder => "Save Report — Choose Destination Folder", "Enregistrer le rapport — Choisir le dossier de destination", "Gem rapport — Vælg destinationsmappe";
    new_report_folder => "New Report — Choose Destination Folder", "Nouveau rapport — Choisir le dossier de destination", "Ny rapport — Vælg destinationsmappe";
    save_environment => "Save Environment…", "Enregistrer l'environnement…", "Gem miljø…";
    save_response => "Save Response…", "Enregistrer la réponse…", "Gem svar…";
    file_saved => "Saved.", "Enregistré.", "Gemt.";
    file_loaded => "Loaded.", "Chargé.", "Indlæst.";
    file_no_response => "No response to save.", "Aucune réponse à enregistrer.", "Intet svar at gemme.";
    file_error_prefix => "Error:", "Erreur :", "Fejl:";
    file_not_collection => "Not a valid collection file (no requests found).", "Fichier de collection invalide (aucune requête trouvée).", "Ikke en gyldig samlingsfil (ingen anmodninger fundet).";
    file_not_collection_prefix => "Not a valid collection file —", "Fichier de collection invalide —", "Ikke en gyldig samlingsfil —";
    save_unreadable_empty_file => "Won't save — the multipart file field '{field}' in '{req}' has no file path, which PaperBoy couldn't read back. Pick a file or remove the field.", "Enregistrement refusé — le champ fichier multipart « {field} » dans « {req} » n'a pas de chemin, que PaperBoy ne pourrait pas relire. Choisissez un fichier ou supprimez le champ.", "Gemmer ikke — multipart-filfeltet '{field}' i '{req}' har ingen filsti, som PaperBoy ikke kunne læse igen. Vælg en fil, eller fjern feltet.";
    file_not_environment => "Not a valid environment file (expected KEY=value lines).", "Fichier d'environnement invalide (lignes CLÉ=valeur attendues).", "Ikke en gyldig miljøfil (forventede NØGLE=værdi-linjer).";
    open_workspace => "Choose Workspace Folder…", "Choisir le dossier Workspace…", "Vælg Workspace-mappe…";
    browser_hint_workspace => "Enter open folder · Space choose as Workspace · ← parent · type to filter · ^r reset · Esc cancel", "Entrée ouvrir dossier · Espace choisir comme Workspace · ← dossier parent · taper pour filtrer · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Mellemrum vælg som Workspace · ← overordnet · skriv for at filtrere · ^r nulstil · Esc annuller";
    workspace_empty_state => "No collection — press w.", "Aucune collection — appuyez sur w.", "Ingen samling — tryk w.";
    foot_workspace => "browse workspace", "parcourir workspace", "gennemse workspace";
    workspace_picker_title => "Workspace", "Workspace", "Workspace";
    workspace_picker_hint => "Enter open · n new collection · R new report · Tab filter · ↑↓ move · Esc cancel", "Entrée ouvrir · n nouvelle collection · R nouveau rapport · Tab filtre · ↑↓ déplacer · Échap annuler", "Enter åbn · n ny samling · R ny rapport · Tab filter · ↑↓ flyt · Esc annuller";
    workspace_picker_hint_add => "Enter add request here · n new collection · Tab toggle filter · ↑↓ move · Esc cancel", "Entrée ajouter la requête ici · n nouvelle collection · Tab basculer filtre · ↑↓ déplacer · Échap annuler", "Enter tilføj forespørgsel her · n ny samling · Tab skift filter · ↑↓ flyt · Esc annuller";
    workspace_picker_hint_move => "Enter move request here · Tab toggle filter · ↑↓ move · Esc cancel", "Entrée déplacer la requête ici · Tab basculer filtre · ↑↓ déplacer · Échap annuler", "Enter flyt forespørgsel her · Tab skift filter · ↑↓ flyt · Esc annuller";
    workspace_picker_hint_copy => "Enter copy request here · Tab toggle filter · ↑↓ move · Esc cancel", "Entrée copier la requête ici · Tab basculer filtre · ↑↓ déplacer · Échap annuler", "Enter kopiér forespørgsel her · Tab skift filter · ↑↓ flyt · Esc annuller";
    request_deleted => "{m} request deleted. Press (u) to restore request.", "Requête {m} supprimée. Appuyez sur (u) pour la restaurer.", "{m}-forespørgsel slettet. Tryk (u) for at gendanne.";
    tab_closed => "Collection closed. Press (u) to reopen tab.", "Collection fermée. Appuyez sur (u) pour rouvrir l'onglet.", "Samling lukket. Tryk (u) for at genåbne fanen.";
    env_deleted => "Environment '{n}' deleted. Press (u) to reopen environment.", "Environnement «\u{a0}{n}\u{a0}» supprimé. Appuyez sur (u) pour le rouvrir.", "Miljøet '{n}' slettet. Tryk (u) for at genåbne miljøet.";
    env_reopened => "Environment '{n}' reopened.", "Environnement «\u{a0}{n}\u{a0}» rouvert.", "Miljøet '{n}' genåbnet.";
    request_moved => "{m} request moved to {dest}.", "Requête {m} déplacée vers {dest}.", "{m}-forespørgsel flyttet til {dest}.";
    request_copied => "{m} request copied to {dest}.", "Requête {m} copiée vers {dest}.", "{m}-forespørgsel kopieret til {dest}.";
    workspace_new_collection_title => "New collection (path relative to workspace)", "Nouvelle collection (chemin relatif au workspace)", "Ny samling (sti relativ til workspace)";
    workspace_collection_created => "New collection '{name}' created — Ctrl+S to save.", "Nouvelle collection « {name} » créée — Ctrl+S pour enregistrer.", "Ny samling '{name}' oprettet — Ctrl+S for at gemme.";
    workspace_report_created => "New report '{name}' created.", "Nouveau rapport « {name} » créé.", "Ny rapport '{name}' oprettet.";
    ws_item_moved => "Moved '{name}'.", "« {name} » déplacé.", "'{name}' flyttet.";
    ws_item_move_exists => "There is already a '{name}' there — nothing moved.", "Il y a déjà un « {name } » à cet endroit — rien n'a été déplacé.", "Der er allerede en '{name}' der — intet flyttet.";
    ws_item_move_into_itself => "A folder can't be moved inside itself.", "Un dossier ne peut pas être déplacé dans lui-même.", "En mappe kan ikke flyttes ind i sig selv.";
    workspace_new_item_title => "New file (.hurl, .trail or .vars)", "Nouveau fichier (.hurl, .trail ou .vars)", "Ny fil (.hurl, .trail eller .vars)";
    ws_item_unknown_kind => "'{name}' isn't a collection, report or environment — use .hurl, .trail or .vars.", "« {name} » n'est ni une collection, ni un rapport, ni un environnement — utilisez .hurl, .trail ou .vars.", "'{name}' er hverken en samling, rapport eller et miljø — brug .hurl, .trail eller .vars.";
    ws_item_created => "Created '{name}'.", "« {name} » créé.", "'{name}' oprettet.";
    ws_item_escaped => "Destination '{name}' resolves outside the workspace — nothing created.", "La destination « {name} » pointe hors de l'espace de travail — rien n'a été créé.", "Destinationen '{name}' peger uden for arbejdsområdet — intet oprettet.";
    ws_item_exists => "'{name}' already exists — nothing created.", "« {name} » existe déjà — rien n'a été créé.", "'{name}' findes allerede — intet oprettet.";
    workspace_report_escaped => "Destination '{name}' resolves outside the workspace — report not created.", "La destination « {name} » pointe hors du workspace — rapport non créé.", "Destinationen '{name}' peger uden for workspacet — rapport ikke oprettet.";
    new_request_url_required => "Can't save: the request needs a URL.", "Impossible d'enregistrer : la requête nécessite une URL.", "Kan ikke gemme: forespørgslen kræver en URL.";
    workspace_filter_on => "Filter: .hurl/.json/.vars/.trail", "Filtre : .hurl/.json/.vars/.trail", "Filter: .hurl/.json/.vars/.trail";
    workspace_filter_off => "Filter: All files", "Filtre : tous les fichiers", "Filter: Alle filer";
    workspace_tree_filter_on => "Tree filter on: .hurl/.json/.vars/.trail only", "Filtre de l'arbre activé : .hurl/.json/.vars/.trail uniquement", "Træfilter til: kun .hurl/.json/.vars/.trail";
    workspace_tree_filter_off => "Tree filter off: showing all files", "Filtre de l'arbre désactivé : tous les fichiers affichés", "Træfilter fra: viser alle filer";
    workspace_no_files => "No matching files in this folder.", "Aucun fichier correspondant dans ce dossier.", "Ingen matchende filer i denne mappe.";
    tab_request => "Scratch Space", "Brouillon", "Kladde";
    run_entry => "▶ Run", "▶ Exécuter", "▶ Kør";
    entry_request_json => "Request JSON", "JSON de la requête", "Anmodnings-JSON";
    entry_request_hurl => "Request Hurl", "Requête (Hurl)", "Anmodning (Hurl)";
    entry_raw_hurl => "Raw Mode (Hurl)", "Mode brut (Hurl)", "Rå tilstand (Hurl)";
    entry_raw_json => "Raw Mode (JSON)", "Mode brut (JSON)", "Rå tilstand (JSON)";
    invalid_hurl => "Not valid Hurl (expected exactly one request); edit and try again.", "Hurl invalide (une seule requête attendue) ; modifiez et réessayez.", "Ikke gyldig Hurl (forventede præcis én anmodning); ret og prøv igen.";
    invalid_hurl_prefix => "Not valid Hurl —", "Hurl invalide —", "Ikke gyldig Hurl —";
    status_copy_key => "^y", "^y", "^y";
    status_copy_hint => "copy", "copier", "kopiér";
    invalid_request_json => "Not valid Request JSON (expected an object with at least \"method\" and \"url\"); edit and try again.", "JSON de requête invalide (un objet avec au moins « method » et « url » est attendu) ; modifiez et réessayez.", "Ikke gyldig anmodnings-JSON (forventede et objekt med mindst \"method\" og \"url\"); ret og prøv igen.";
    no_requests_hint => "No requests yet. Use \u{FF0B} New Request to create one.", "Aucune requête. Utilisez \u{FF0B} Nouvelle requête pour en créer une.", "Ingen anmodninger endnu. Brug \u{FF0B} Ny anmodning for at oprette en.";
    list_up_row => "‹ .. (up a folder)", "‹ .. (dossier parent)", "‹ .. (mappe op)";
    new_request => "\u{FF0B} New Request", "\u{FF0B} Nouvelle requête", "\u{FF0B} Ny anmodning";
    edit_request => "\u{270E} Edit Request", "\u{270E} Modifier la requête", "\u{270E} Rediger anmodning";
    field_name => "Name", "Nom", "Navn";
    field_target => "Add to", "Ajouter à", "Tilføj til";
    field_method => "Method", "Méthode", "Metode";
    field_url => "URL", "URL", "URL";
    field_headers => "Headers", "En-têtes", "Headere";
    field_cookies => "Cookies", "Cookies", "Cookies";
    field_queries => "Queries", "Requêtes", "Forespørgsler";
    field_form => "Form", "Formulaire", "Formular";
    field_options => "Options", "Options", "Indstillinger";
    field_body => "Body", "Corps", "Brødtekst";
    field_asserts => "Asserts", "Assertions", "Assertions";
    field_captures => "Captures", "Captures", "Captures";
    field_reports => "Reports", "Rapports", "Rapporter";
    tab_all => "All", "Tout", "Alle";
    hdr_key => "Key", "Clé", "Nøgle";
    hdr_value => "Value", "Valeur", "Værdi";
    hdr_description => "Description", "Description", "Beskrivelse";
    hdr_name => "Name", "Nom", "Navn";
    hdr_option => "Option", "Option", "Indstilling";
    hdr_query => "Query", "Requête", "Forespørgsel";
    hdr_type => "Type", "Type", "Type";
    form_type_text => "Text", "Texte", "Tekst";
    form_type_file => "File", "Fichier", "Fil";
    form_type_base64file => "Base64 File", "Fichier Base64", "Base64-fil";
    hdr_base64_prefix => "Base64 Prefix", "Préfixe Base64", "Base64-præfiks";
    content_type_hint => "Content-Type", "Type de contenu", "Content-Type";
    content_type_auto => "Auto (detect from extension)", "Auto (détecter depuis l'extension)", "Auto (registrer fra filtype)";
    content_type_auto_placeholder => "Auto", "Auto", "Auto";
    hint_pick_file => "^F browse", "^F parcourir", "^F gennemse";
    hint_delete_row => "^D delete row", "^D supprimer la ligne", "^D slet række";
    hint_toggle_enabled => "^E toggle enabled", "^E activer/désactiver", "^E slå til/fra";
    add_header => "\u{FF0B} Add header", "\u{FF0B} Ajouter un en-tête", "\u{FF0B} Tilføj header";
    add_cookie => "\u{FF0B} Add cookie", "\u{FF0B} Ajouter un cookie", "\u{FF0B} Tilføj cookie";
    add_query => "\u{FF0B} Add query", "\u{FF0B} Ajouter une requête", "\u{FF0B} Tilføj forespørgsel";
    add_option => "\u{FF0B} Add option", "\u{FF0B} Ajouter une option", "\u{FF0B} Tilføj indstilling";
    add_form_field => "\u{FF0B} Add field", "\u{FF0B} Ajouter un champ", "\u{FF0B} Tilføj felt";
    add_assert => "\u{FF0B} Add assert", "\u{FF0B} Ajouter une assertion", "\u{FF0B} Tilføj assertion";
    add_capture => "\u{FF0B} Add capture", "\u{FF0B} Ajouter une capture", "\u{FF0B} Tilføj capture";
    add_report => "\u{FF0B} Add report field", "\u{FF0B} Ajouter un champ de rapport", "\u{FF0B} Tilføj rapportfelt";
    cap_name => "Name", "Nom", "Navn";
    cap_expr => "Expression", "Expression", "Udtryk";
    report_name => "Name", "Nom", "Navn";
    report_expr => "Expression", "Expression", "Udtryk";
    load_environment => "Load Environment…", "Charger l'environnement…", "Indlæs miljø…";
    env_heading => "Global Environments", "Environnements globaux", "Globale miljøer";
    env_no_env => "(no environment loaded)", "(aucun environnement chargé)", "(intet miljø indlæst)";
    env_loading => "Loading secret…", "Chargement du secret…", "Indlæser hemmelighed…";
    env_waiting_secrets => "Waiting for secrets:", "En attente des secrets\u{a0}:", "Venter på hemmeligheder:";
    env_reloading_var => "Reloading", "Rechargement de", "Genindlæser";
    env_activated => "Activated", "Activé", "Aktiveret";
    env_deactivated => "Deactivated", "Désactivé", "Deaktiveret";
    env_rename_title => "Rename Environment", "Renommer l'environnement", "Omdøb miljø";
    env_link_picker_title => "Link Environment", "Lier un environnement", "Tilknyt miljø";
    env_link_none => "(none)", "(aucun)", "(ingen)";
    env_delete_confirm => "Delete this environment?", "Supprimer cet environnement\u{a0}?", "Slet dette miljø?";
    env_no_envs => "(no environments — Load Environment… to add one)", "(aucun environnement — Charger l'environnement… pour en ajouter un)", "(ingen miljøer — Indlæs miljø… for at tilføje et)";
    env_active_label => "Active: ", "Actif : ", "Aktiv: ";
    env_active_none => "(none active)", "(aucun actif)", "(intet aktivt)";
    env_filter_label => "Filter: ", "Filtre : ", "Filter: ";
    env_filter_no_matches => "No environment matches — Esc clears the filter.", "Aucun environnement ne correspond — Échap efface le filtre.", "Intet miljø matcher — Esc rydder filteret.";
    env_source_label => "Source: ", "Source : ", "Kilde: ";
    env_source_both => "Both", "Les deux", "Begge";
    env_source_global => "Global", "Global", "Global";
    env_source_workspace => "Workspace", "Workspace", "Workspace";
    env_source_no_matches => "No environments from this source.", "Aucun environnement de cette source.", "Ingen miljøer fra denne kilde.";
    foot_env_filter => "filter", "filtrer", "filtrér";
    env_collision_title => "Environment name already exists", "Ce nom d'environnement existe déjà", "Miljønavnet findes allerede";
    env_collision_replace => "Replace existing", "Remplacer l'existant", "Erstat eksisterende";
    env_collision_keep_both => "Keep both (duplicate name)", "Conserver les deux (nom en double)", "Behold begge (dublet navn)";
    env_collision_abort => "Abort", "Annuler", "Afbryd";
    env_collision_rename => "Rename then add", "Renommer puis ajouter", "Omdøb og tilføj";
    run_summary_passed => "Passed", "Réussi", "Bestået";
    run_summary_failed => "Failed", "Échoué", "Fejlet";
    run_summary_total => "Total", "Total", "Total";
    run_all_streaming_cookies => "Run All: streaming — cookies aren't carried between requests (use batch mode in Preferences)", "Tout exécuter : diffusion — les cookies ne sont pas transmis entre les requêtes (utilisez le mode par lots dans les Préférences)", "Kør alle: streaming — cookies overføres ikke mellem forespørgsler (brug batch-tilstand i Indstillinger)";
    env_add_var_title => "New environment variable", "Nouvelle variable d'environnement", "Ny miljøvariabel";
    env_var_switch => "switch", "changer", "skift";
    env_still_secret => "Still secret", "Toujours secret", "Stadig hemmelig";
    env_still_secret_hint => "Ctrl+T: toggle still-secret", "Ctrl+T\u{a0}: bascule toujours-secret", "Ctrl+T: skift stadig-hemmelig";
    git_collection_menu => "Load Collection from Git…", "Charger une collection depuis Git…", "Indlæs samling fra Git…";
    git_env_menu => "Load Environment from Git…", "Charger un environnement depuis Git…", "Indlæs miljø fra Git…";
    git_workspace_menu => "Load Workspace from Git…", "Charger un Workspace depuis Git…", "Indlæs Workspace fra Git…";
    git_report_menu => "Load Report from Git…", "Charger un rapport depuis Git…", "Indlæs rapport fra Git…";
    git_url_label => "Git URL", "URL Git", "Git-URL";
    git_url_hint => "https://github.com/owner/repo", "https://github.com/owner/repo", "https://github.com/owner/repo";
    git_token_label => "Token", "Jeton", "Token";
    git_token_hint => "blank for a public repo", "vide pour un dépôt public", "tom for et offentligt repo";
    git_connect_hint => "Enter connect · Esc cancel", "Entrée connecter · Échap annuler", "Enter forbind · Esc annuller";
    gui_git_recent => "Recent", "Récents", "Seneste";
    git_recent_hint => "↓ recent URLs", "↓ URL récentes", "↓ seneste URL'er";
    git_pick_ref_title => "Select a branch or tag", "Sélectionnez une branche ou une étiquette", "Vælg en gren eller et tag";
    git_pick_file_title => "Select a file", "Sélectionnez un fichier", "Vælg en fil";
    git_filter_hint => "Type to filter · Enter select · Esc cancel", "Filtrer en tapant · Entrée choisir · Échap annuler", "Skriv for at filtrere · Enter vælg · Esc annuller";
    git_loading_refs => "Fetching branches and tags…", "Récupération des branches et étiquettes…", "Henter grene og tags…";
    git_loading_files => "Fetching file list…", "Récupération de la liste des fichiers…", "Henter filliste…";
    git_loading_file => "Fetching file…", "Récupération du fichier…", "Henter fil…";
    git_loading_workspace_files => "Downloading matching files…", "Téléchargement des fichiers correspondants…", "Henter matchende filer…";
    git_loading_hint => "(Esc to cancel)", "(Échap pour annuler)", "(Esc for at annullere)";
    git_error_hint => "Esc close", "Échap fermer", "Esc luk";
    git_url_required => "A Git URL is required.", "Une URL Git est requise.", "En Git-URL er påkrævet.";
    git_branches => "Branches", "Branches", "Grene";
    git_tags => "Tags", "Étiquettes", "Tags";
    git_filter_label => "filter: ", "filtre\u{a0}: ", "filter: ";
    git_pick_workspace_filter_title => "Choose which files to download", "Choisissez les fichiers à télécharger", "Vælg hvilke filer der skal hentes";
    git_workspace_filter_hint => "Enter select · Esc cancel", "Entrée choisir · Échap annuler", "Enter vælg · Esc annuller";
    git_ws_filter_hurl_json => ".hurl and .json files (recommended)", "Fichiers .hurl et .json (recommandé)", ".hurl- og .json-filer (anbefalet)";
    git_ws_filter_hurl => ".hurl files only", "Fichiers .hurl uniquement", "Kun .hurl-filer";
    git_ws_filter_json => ".json files only", "Fichiers .json uniquement", "Kun .json-filer";
    git_ws_filter_all => "All files", "Tous les fichiers", "Alle filer";
    git_workspace_no_matches => "No files in this repo matched that filter.", "Aucun fichier de ce dépôt ne correspond à ce filtre.", "Ingen filer i dette repo matchede filteret.";
    close_git_workspace_q => "This Workspace's files were downloaded from git into:\n{p}\n\nKeep this folder so the tab can be reopened later, or delete it now?", "Les fichiers de cet Espace de travail ont été téléchargés depuis git dans :\n{p}\n\nConserver ce dossier pour pouvoir rouvrir l'onglet plus tard, ou le supprimer maintenant ?", "Denne Workspaces filer blev downloadet fra git til:\n{p}\n\nBehold denne mappe, så fanen kan genåbnes senere, eller slet den nu?";
    close_git_workspace_keep => "Keep", "Conserver", "Behold";
    close_git_workspace_delete => "Delete", "Supprimer", "Slet";
    close_git_workspace_cancel => "Cancel", "Annuler", "Annuller";
    workspace_folder_missing => "The folder for Workspace '{name}' could not be found (it may have been cleared since your last session) and has been reset — pick a folder, or Load Workspace from Git again.", "Le dossier de l'Espace de travail « {name} » est introuvable (il a peut-être été supprimé depuis votre dernière session) et a été réinitialisé — choisissez un dossier, ou chargez à nouveau l'Espace de travail depuis Git.", "Mappen for Workspace '{name}' kunne ikke findes (den er muligvis blevet ryddet siden din sidste session) og er blevet nulstillet — vælg en mappe, eller indlæs Workspace fra Git igen.";
    workspace_reload_confirm_q => "Workspace '{name}''s downloaded files are missing (likely cleared from a temp folder). Try to redownload {ref} from:\n{url}?", "Les fichiers téléchargés de l'Espace de travail « {name} » sont introuvables (probablement supprimés d'un dossier temporaire). Essayer de retélécharger {ref} depuis :\n{url} ?", "De downloadede filer til Workspace '{name}' mangler (sandsynligvis ryddet fra en midlertidig mappe). Prøv at downloade {ref} igen fra:\n{url}?";
    workspace_reload_loading => "Redownloading workspace from git…", "Retéléchargement de l'espace de travail depuis git…", "Downloader workspace igen fra git…";
    workspace_reload_success => "Workspace redownloaded from git.", "Espace de travail retéléchargé depuis git.", "Workspace downloadet igen fra git.";
    workspace_reload_failed => "Could not redownload the workspace — the remote no longer seems to have that commit or tag ({e}).", "Impossible de retélécharger l'espace de travail — le dépôt distant ne semble plus avoir ce commit ou ce tag ({e}).", "Kunne ikke downloade workspace igen — det fjerne repo synes ikke længere at have denne commit eller tag ({e}).";
    workspace_reload_save_hint => "Tip: save this Workspace to a permanent local folder if you want it to always be available without redownloading.", "Astuce : enregistrez cet Espace de travail dans un dossier local permanent si vous voulez qu'il soit toujours disponible sans nouveau téléchargement.", "Tip: gem denne Workspace i en permanent lokal mappe, hvis du vil have den altid tilgængelig uden at skulle downloade igen.";
    file_not_workspace => "The active tab isn't a Workspace.", "L'onglet actif n'est pas un Workspace.", "Den aktive fane er ikke en Workspace.";
    move_workspace_item => "Move — Choose Destination Folder", "Déplacer — Choisir le dossier de destination", "Flyt — Vælg destinationsmappe";
    browser_hint_header_folder => "Enter open folder · Space choose as report root · ← parent · type to filter · ^r reset · Esc cancel", "Entrée ouvrir dossier · Espace choisir comme racine du rapport · ← dossier parent · taper pour filtrer · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Mellemrum vælg som rapportrod · ← overordnet · skriv for at filtrere · ^r nulstil · Esc annuller";
    browser_hint_node_folder => "Enter open folder · Space choose this folder · ← parent · type to filter · ^r reset · Esc cancel", "Entrée ouvrir dossier · Espace choisir ce dossier · ← dossier parent · taper pour filtrer · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Mellemrum vælg denne mappe · ← overordnet · skriv for at filtrere · ^r nulstil · Esc annuller";
    browser_hint_workspace_move => "Enter open folder · Space move here · ← parent · type to filter · ^r reset · Esc cancel", "Entrée ouvrir dossier · Espace déplacer ici · ← dossier parent · taper pour filtrer · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Mellemrum flyt hertil · ← overordnet · skriv for at filtrere · ^r nulstil · Esc annuller";
    save_workspace => "Save Workspace — Choose Destination Folder", "Enregistrer le Workspace — Choisir le dossier de destination", "Gem Workspace — Vælg destinationsmappe";
    save_collection_folder => "Save Collection — Choose Destination Folder", "Enregistrer la collection — Choisir le dossier de destination", "Gem samling — Vælg destinationsmappe";
    browser_hint_collection_save => "Enter open folder · Space save here · Tab rename · ← parent · type to filter · ^r reset · Esc cancel", "Entrée ouvrir dossier · Espace enregistrer ici · Tab renommer · ← dossier parent · taper pour filtrer · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Mellemrum gem her · Tab omdøb · ← overordnet · skriv for at filtrere · ^r nulstil · Esc annuller";
    browser_hint_new_report => "Enter open folder · Space create here · Tab name (use sub/name for a new folder) · type to filter · ^n scratch tab · ← up · Esc cancel", "Entrée ouvrir dossier · Espace créer ici · Tab nom (sous-dossier/nom pour créer un dossier) · taper pour filtrer · ^n onglet brouillon · ← remonter · Échap annuler", "Enter åbn mappe · Mellemrum opret her · Tab navn (undermappe/navn opretter en mappe) · skriv for at filtrere · ^n kladdefane · ← op · Esc annuller";
    browser_hint_workspace_save => "Enter open folder · Space save here · Tab rename · ← parent · type to filter · ^r reset · Esc cancel", "Entrée ouvrir dossier · Espace enregistrer ici · Tab renommer · ← dossier parent · taper pour filtrer · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Mellemrum gem her · Tab omdøb · ← overordnet · skriv for at filtrere · ^r nulstil · Esc annuller";
    browser_filename_label => "File name", "Nom du fichier", "Filnavn";
    browser_foldername_label => "Folder name", "Nom du dossier", "Mappenavn";
    browser_name_hint => "Enter save · Esc back to list", "Entrée enregistrer · Échap retour à la liste", "Enter gem · Esc tilbage til listen";
    browser_filter_label => "Filter: ", "Filtre : ", "Filter: ";
    browser_no_matches => "No matching files", "Aucun fichier correspondant", "Ingen matchende filer";
    workspace_save_success => "Workspace saved.", "Workspace enregistré.", "Workspace gemt.";
    workspace_save_failed => "Could not save the workspace ({e}).", "Impossible d'enregistrer le Workspace ({e}).", "Kunne ikke gemme workspace ({e}).";
    git_workspace_storage_q => "Workspace downloaded. Keep it in a temporary folder, or save it to a permanent location now?", "Workspace téléchargé. Le garder dans un dossier temporaire, ou l'enregistrer dans un emplacement permanent maintenant ?", "Workspace downloadet. Behold den i en midlertidig mappe, eller gem den på en permanent placering nu?";
    git_workspace_storage_temp => "Keep temporarily", "Garder temporairement", "Behold midlertidigt";
    git_workspace_storage_choose => "Choose a folder…", "Choisir un dossier…", "Vælg en mappe…";
    git_no_origin => "This collection wasn't loaded from Git.", "Cette collection n'a pas été chargée depuis Git.", "Denne samling blev ikke indlæst fra Git.";
    git_save_title => "Save to Git", "Enregistrer sur Git", "Gem til Git";
    git_save_workspace_empty => "The workspace has no files to save.", "Le workspace n'a aucun fichier à enregistrer.", "Dette workspace har ingen filer at gemme.";
    git_save_source_gone => "That tab was closed — there is nothing left to save.", "Cet onglet a été fermé — il n'y a plus rien à enregistrer.", "Fanen blev lukket — der er ikke længere noget at gemme.";
    gui_git_err_ws_not_from_git => "This workspace was not downloaded from Git, so there is nowhere to push it back to.", "Ce workspace n'a pas été téléchargé depuis Git, il n'y a donc nulle part où le renvoyer.", "Dette workspace blev ikke hentet fra Git, så der er ingen steder at sende det tilbage til.";
    git_save_ws_unsaved_q => "The current file has unsaved changes. A Git push commits the files as they are on disk, so those changes won't be included unless you save them first.\n\nWhat would you like to do?", "Le fichier actuel a des modifications non enregistrées. Un envoi vers Git valide les fichiers tels qu'ils sont sur le disque, donc ces modifications ne seront pas incluses si vous ne les enregistrez pas d'abord.\n\nQue souhaitez-vous faire ?", "Den aktuelle fil har ugemte ændringer. En Git-push committer filerne, som de er på disken, så disse ændringer medtages ikke, medmindre du gemmer dem først.\n\nHvad vil du gøre?";
    git_save_ws_unsaved_save => "Save changes, then push", "Enregistrer les modifications, puis envoyer", "Gem ændringer, og push derefter";
    git_save_ws_unsaved_ignore => "Push the saved version (discard unsaved changes)", "Envoyer la version enregistrée (ignorer les modifications non enregistrées)", "Push den gemte version (kassér ugemte ændringer)";
    git_save_ws_unsaved_cancel => "Cancel", "Annuler", "Annuller";
    ws_switch_unsaved_q => "The current collection has unsaved changes. Opening a different collection replaces them in memory, so they will be lost unless you save them first.\n\nWhat would you like to do?", "La collection actuelle a des modifications non enregistrées. Ouvrir une autre collection les remplace en mémoire, elles seront donc perdues si vous ne les enregistrez pas d'abord.\n\nQue souhaitez-vous faire ?", "Den aktuelle samling har ugemte ændringer. Hvis du åbner en anden samling, erstattes de i hukommelsen, så de går tabt, medmindre du gemmer dem først.\n\nHvad vil du gøre?";
    ws_switch_unsaved_save => "Save changes, then switch", "Enregistrer les modifications, puis changer", "Gem ændringer, og skift derefter";
    ws_switch_unsaved_discard => "Discard changes and switch", "Ignorer les modifications et changer", "Kassér ændringer og skift";
    ws_switch_unsaved_cancel => "Cancel", "Annuler", "Annuller";
    git_save_include_env_label => "Also save the environment", "Enregistrer aussi l'environnement", "Gem også miljøet";
    git_save_collection_path_label => "Collection path in repo", "Chemin de la collection dans le dépôt", "Samlingens sti i repoet";
    git_save_report_path_label => "Report path in repo", "Chemin du rapport dans le dépôt", "Rapportens sti i repoet";
    git_save_env_path_label => "Environment path in repo", "Chemin de l'environnement dans le dépôt", "Miljøets sti i repoet";
    git_save_step_hint => "Enter continue · Esc cancel", "Entrée continuer · Échap annuler", "Enter fortsæt · Esc annuller";
    git_save_branch_label => "Branch", "Branche", "Gren";
    git_save_tag_label => "Tag", "Étiquette", "Tag";
    git_save_target_hint => "Tab Branch/Tag · Enter continue · Esc cancel", "Tab Branche/Étiquette · Entrée continuer · Échap annuler", "Tab Gren/Tag · Enter fortsæt · Esc annuller";
    git_save_commit_msg_label => "Commit message", "Message de commit", "Commit-besked";
    git_save_commit_msg_hint => "Enter push · Esc cancel", "Entrée pousser · Échap annuler", "Enter push · Esc annuller";
    git_save_pushing => "Pushing to Git…", "Envoi vers Git…", "Sender til Git…";
    git_save_success => "Saved to Git", "Enregistré sur Git", "Gemt til Git";
    git_tag_exists => "That tag already exists — tags are never overwritten. Choose a different name.", "Cette étiquette existe déjà — les étiquettes ne sont jamais écrasées. Choisissez un autre nom.", "Det tag findes allerede — tags bliver aldrig overskrevet. Vælg et andet navn.";
    git_ref_exists_race => "That name was just created by someone else — pick it from the list, or choose a different one.", "Ce nom vient d'être créé par quelqu'un d'autre — choisissez-le dans la liste, ou choisissez-en un autre.", "Det navn blev lige oprettet af en anden — vælg det fra listen, eller vælg et andet.";
    json_enter_to_edit => "to edit", "pour modifier", "for at redigere";
    subst_hint_loaded => "loaded", "chargé", "indlæst";
    subst_hint_literal => "literal", "littéral", "literal";
    subst_hint_loading => "loading", "en cours", "indlæser";
    subst_hint_missing => "missing", "manquant", "mangler";
    subst_hint_shadowed => "shadowed by linked env", "masqué par l'environnement lié", "skygget af tilknyttet miljø";
    json_invalid => "⚠ Invalid JSON — fix before running", "⚠ JSON invalide — corrigez avant d'exécuter", "⚠ Ugyldig JSON — ret før kørsel";
    foot_focus => "focus", "focus", "fokus";
    foot_move => "move", "déplacer", "flyt";
    foot_edit => "edit", "modifier", "rediger";
    foot_run => "run", "exécuter", "kør";
    foot_run_all => "run all", "tout exécuter", "kør alle";
    foot_env_activate => "activate/deactivate", "activer/désactiver", "aktivér/deaktivér";
    foot_env_link => "link env", "lier env", "link miljø";
    foot_new => "New Request/Var", "Nouvelle requête/var", "Ny forespørgsel/var";
    foot_rename => "rename", "renommer", "omdøb";
    foot_close => "delete", "supprimer", "fjern";
    foot_copy_selection => "copy", "copier", "kopiér";
    foot_compact => "compact", "compact", "kompakt";
    foot_help => "help", "aide", "hjælp";
    foot_quit => "quit", "quitter", "afslut";
    help_title => "Help", "Aide", "Hjælp";
    help_heading => "PaperBoy — Terminal UI", "PaperBoy — Interface Terminal", "PaperBoy — Terminalgrænseflade";
    help_tab_shortcuts => "Shortcuts", "Raccourcis", "Genveje";
    help_tab_glossary => "Glossary", "Glossaire", "Ordliste";
    help_tab_switch_hint => "Tab / ←→ switch view · type to filter", "Tab / ←→ changer de vue · tapez pour filtrer", "Tab / ←→ skift visning · skriv for at filtrere";
    help_filter_label => "Filter: ", "Filtre : ", "Filter: ";
    help_filter_no_matches => "No entries match — the filter applies per tab, so try another view (Tab / ←→).", "Aucune entrée ne correspond — le filtre s'applique par onglet, essayez une autre vue (Tab / ←→).", "Ingen poster matcher — filteret gælder pr. fane, så prøv en anden visning (Tab / ←→).";
    glossary_heading => "Substitution colours & icons — Request JSON/Hurl view", "Couleurs et icônes de substitution — vue Requête JSON/Hurl", "Substitutionsfarver & ikoner — Request JSON/Hurl-visning";
    glossary_label_literal => "literal", "littéral", "literal";
    glossary_desc_literal => "A plain literal value from the active Environment, substituted directly.", "Une valeur littérale simple de l'environnement actif, substituée directement.", "En simpel literal værdi fra det aktive miljø, indsat direkte.";
    glossary_label_loaded => "loaded", "chargé", "indlæst";
    glossary_desc_loaded => "Resolved from an external source (environment variable, 1Password, SSM) or an initialised response capture.", "Résolu depuis une source externe (variable d'environnement, 1Password, SSM) ou une capture de réponse initialisée.", "Hentet fra en ekstern kilde (miljøvariabel, 1Password, SSM) eller et initialiseret svar-fangst.";
    glossary_label_pending => "loading", "en cours", "indlæser";
    glossary_desc_pending => "A secret reference still being fetched in the background; kept as \"{{ VAR }}\" until it resolves.", "Une référence à un secret encore en cours de récupération en arrière-plan\u{a0}; reste affichée sous forme \"{{ VAR }}\" jusqu'à sa résolution.", "En hemmelighedsreference, der stadig hentes i baggrunden; vises som \"{{ VAR }}\" indtil den er løst.";
    glossary_label_failed => "missing", "manquant", "mangler";
    glossary_desc_failed => "Failed to resolve, or a response capture not yet initialised — kept as \"{{ VAR }}\".", "Échec de la résolution, ou capture de réponse pas encore initialisée — reste affichée sous forme \"{{ VAR }}\".", "Kunne ikke løses, eller en svar-fangst der endnu ikke er initialiseret — vises som \"{{ VAR }}\".";
    glossary_label_shadowed => "shadowed", "masqué", "skygget";
    glossary_desc_shadowed => "This value comes from the active Global Environment, but is being overridden by the collection's linked Environment — the linked value is the one actually substituted.", "Cette valeur provient de l'environnement global actif, mais est masquée par l'environnement lié de la collection — c'est la valeur liée qui est réellement substituée.", "Denne værdi kommer fra det aktive globale miljø, men bliver overskygget af samlingens tilknyttede miljø — det er den tilknyttede værdi, der faktisk indsættes.";
    glossary_heading_icons => "Other icons used throughout the app", "Autres icônes utilisées dans l'application", "Andre ikoner brugt i appen";
    glossary_label_modified => "modified", "modifié", "ændret";
    glossary_desc_modified => "A pencil marks a request, header, or variable that has been edited away from its originally loaded value.", "Un crayon marque une requête, un en-tête ou une variable modifiée par rapport à sa valeur chargée d'origine.", "En blyant markerer en request, header eller variabel, der er redigeret væk fra sin oprindeligt indlæste værdi.";
    glossary_label_added => "added", "ajouté", "tilføjet";
    glossary_desc_added => "A plus marks a request or variable added by hand, rather than loaded from a file.", "Un plus marque une requête ou une variable ajoutée manuellement, plutôt que chargée depuis un fichier.", "Et plus markerer en request eller variabel, der er tilføjet manuelt i stedet for indlæst fra en fil.";
    glossary_label_passed => "passed / active", "réussi / actif", "bestået / aktiv";
    glossary_desc_passed => "A request or assertion that passed in the last \"Run All\", or the currently active Global Environment in its list.", "Une requête ou assertion réussie lors du dernier \"Tout exécuter\", ou l'environnement global actuellement actif dans sa liste.", "En request eller assertion, der bestod ved sidste \"Kør alle\", eller det globale miljø, der aktuelt er aktivt i sin liste.";
    glossary_label_run_failed => "failed", "échoué", "fejlede";
    glossary_desc_run_failed => "A request or assertion that failed in the last \"Run All\".", "Une requête ou assertion en échec lors du dernier \"Tout exécuter\".", "En request eller assertion, der fejlede ved sidste \"Kør alle\".";
    glossary_label_running => "running", "en cours", "kører";
    glossary_desc_running => "A request that is still running as part of a batch \"Run All\".", "Une requête encore en cours d'exécution dans un lot \"Tout exécuter\".", "En request, der stadig kører som en del af en batch \"Kør alle\".";
    glossary_label_git => "git-linked", "lié à git", "git-tilknyttet";
    glossary_desc_git => "This Collection, Environment, or Workspace was loaded from, and is linked to, a git remote.", "Cette collection, cet environnement ou cet espace de travail a été chargé depuis, et reste lié à, une origine git distante.", "Denne samling, dette miljø eller denne Workspace blev indlæst fra, og er stadig tilknyttet, en git-fjernserver.";
    glossary_label_linked => "linked environment", "environnement lié", "tilknyttet miljø";
    glossary_desc_linked => "Joins a Collection's tab/title to the Global Environment linked to it.", "Relie l'onglet/titre d'une collection à l'environnement global qui lui est lié.", "Forbinder en samlings faneblad/titel til det globale miljø, den er tilknyttet.";
    glossary_label_folder => "folder", "dossier", "mappe";
    glossary_desc_folder => "A subfolder grouping requests in the list, or — next to a File-type form value — a hint that pressing Enter opens a file picker.", "Un sous-dossier regroupant des requêtes dans la liste, ou — à côté d'une valeur de formulaire de type Fichier — une indication que Entrée ouvre un sélecteur de fichier.", "En undermappe, der grupperer requests i listen, eller — ved siden af en formularværdi af typen Fil — et hint om, at Enter åbner en filvælger.";
    glossary_label_scroll_hint => "more text", "plus de texte", "mere tekst";
    glossary_desc_scroll_hint => "Shown at the edge of a truncated line when there is more hidden text to scroll to in that direction.", "Affiché au bord d'une ligne tronquée lorsqu'il reste du texte masqué à faire défiler dans cette direction.", "Vises i kanten af en afkortet linje, når der er mere skjult tekst at rulle til i den retning.";
    help_focus => "cycle focus between panes", "changer de panneau", "skift mellem paneler";
    help_move => "move within a pane", "se déplacer dans un panneau", "flyt inden for et panel";
    help_page_response => "page up/down through the Response body", "page précédente/suivante dans le corps de la réponse", "side op/ned gennem svar-teksten";
    help_switch_tabs => "switch tabs (Tabs pane)", "changer d'onglet (panneau Onglets)", "skift faner (Faner-panel)";
    help_select => "select / edit focused item (wizard)", "sélectionner / modifier l'élément (assistant)", "vælg / rediger det fokuserede element (guiden)";
    help_run => "send request / run entry", "envoyer la requête / exécuter l'entrée", "send anmodning / kør post";
    help_run_all => "run every request in the collection, in order (like the CLI)", "exécuter toutes les requêtes de la collection, dans l'ordre (comme la CLI)", "kør alle anmodninger i samlingen, i rækkefølge (som CLI'en)";
    help_raw_mode => "edit selected request in Raw Mode (Hurl text)", "modifier la requête sélectionnée en mode brut (texte Hurl)", "rediger den valgte anmodning i råtilstand (Hurl-tekst)";
    help_raw_json => "edit selected request in Raw Mode (JSON text)", "modifier la requête sélectionnée en mode brut (texte JSON)", "rediger den valgte anmodning i råtilstand (JSON-tekst)";
    help_new => "new request (or add variable, in the environment popup)", "nouvelle requête (ou ajouter une variable, dans la popup d'environnement)", "ny anmodning (eller tilføj variabel, i miljø-popup'en)";
    help_base_url => "edit default new-request URL", "modifier l'URL par défaut des nouvelles requêtes", "rediger standard-URL for nye anmodninger";
    help_menus => "File / Settings menu", "menu Fichier / Paramètres", "Fil- / Indstillinger-menu";
    help_workspace_browse => "Browse Workspace (choose a collection file)", "Parcourir le Workspace (choisir un fichier de collection)", "Gennemse Workspace (vælg en samlingsfil)";
    help_workspace_new_item => "new collection / report / environment in the workspace (name it .hurl, .trail or .vars)", "nouvelle collection / rapport / environnement dans l'espace de travail (nommez-le .hurl, .trail ou .vars)", "ny samling / rapport / miljø i arbejdsområdet (navngiv den .hurl, .trail eller .vars)";
    help_workspace_move_item => "move the highlighted workspace file or folder to another folder", "déplacer le fichier ou dossier d'espace de travail en surbrillance vers un autre dossier", "flyt den fremhævede workspace-fil eller -mappe til en anden mappe";
    help_workspace_open => "open the highlighted workspace folder / collection (→ or Enter)", "ouvrir le dossier / la collection du workspace en surbrillance (→ ou Entrée)", "åbn den fremhævede workspace-mappe / -samling (→ eller Enter)";
    help_menu_submenu_nav => "enter / leave a File-menu submenu", "entrer / quitter un sous-menu du menu Fichier", "gå ind i / ud af en Fil-menu-undermenu";
    help_browser_reset => "reset the file browser to the folder it opened in", "réinitialiser l'explorateur de fichiers au dossier d'ouverture", "nulstil filvælgeren til den mappe, den åbnede i";
    help_prev_next_tab => "previous / next tab", "onglet précédent / suivant", "forrige / næste fane";
    help_rename_close => "rename tab (F2) · delete request / close collection tab", "renommer l'onglet (F2) · supprimer la requête / fermer l'onglet", "omdøb fane (F2) · slet anmodning / luk samlingsfane";
    help_reload_var => "reload a failed environment entry (env var / 1Password / SSM)", "recharger une entrée d'environnement en échec (var d'env / 1Password / SSM)", "genindlæs en mislykket miljøvariabel (miljøvariabel / 1Password / SSM)";
    help_env_activate => "activate / deactivate the selected Global Environment", "activer / désactiver l'environnement global sélectionné", "aktivér / deaktivér det valgte globale miljø";
    help_env_delete => "delete the selected Global Environment (unlinks any collections using it)", "supprimer l'environnement global sélectionné (délie les collections qui l'utilisent)", "slet det valgte globale miljø (fjerner link fra samlinger, der bruger det)";
    help_env_reopen => "reopen the most recently deleted Global Environment", "rouvrir l'environnement global supprimé le plus récemment", "genåbn det senest slettede globale miljø";
    help_env_link => "link / unlink a Global Environment to the active collection", "lier / délier un environnement global à la collection active", "link / afkobl et globalt miljø til den aktive samling";
    help_env_view_linked => "view the active collection's linked Global Environment", "afficher l'environnement global lié à la collection active", "vis den aktive samlings tilknyttede globale miljø";
    help_env_rename => "rename the selected Global Environment", "renommer l'environnement global sélectionné", "omdøb det valgte globale miljø";
    help_env_filter => "filter the environments list by name (Esc clears it)", "filtrer la liste des environnements par nom (Échap l'efface)", "filtrér miljølisten efter navn (Esc rydder det)";
    help_env_source => "cycle environment source", "changer la source des environnements", "skift miljøkilde";
    help_env_activate_workspace => "activate the selected workspace environment file", "activer le fichier d'environnement sélectionné de l'espace de travail", "aktivér arbejdsområdets valgte miljøfil";
    help_revert_request => "revert the selected request to its last saved version on disk", "rétablir la requête sélectionnée à sa dernière version enregistrée sur le disque", "gendan den valgte anmodning til dens sidst gemte version på disken";
    help_revert_env => "revert the whole environment to its last saved values on disk", "rétablir tout l'environnement à ses dernières valeurs enregistrées sur le disque", "gendan hele miljøet til dets sidst gemte værdier på disken";
    help_resize => "shrink / grow response pane", "réduire / agrandir le panneau de réponse", "formindsk / forøg svarpanelet";
    help_resize_width => "grow / shrink left column", "agrandir / réduire la colonne de gauche", "forøg / formindsk venstre kolonne";
    help_tab_manage => "close / reopen collection or workspace tab", "fermer / rouvrir un onglet de collection ou d'espace de travail", "luk / genåbn samlings- eller workspace-fane";
    help_tab_reorder => "reorder tabs", "réorganiser les onglets", "omarranger faner";
    help_restore_request => "restore deleted request (List pane)", "restaurer la requête supprimée (volet Liste)", "gendan slettet anmodning (Liste-rude)";
    help_move_request => "move request to another collection (workspace, List pane)", "déplacer la requête vers une autre collection (espace de travail, volet Liste)", "flyt anmodning til en anden samling (arbejdsområde, Liste-rude)";
    help_copy_request => "copy request to another collection (workspace, List pane)", "copier la requête vers une autre collection (espace de travail, volet Liste)", "kopiér anmodning til en anden samling (arbejdsområde, Liste-rude)";
    help_row_toggle_delete => "in wizard tables: ^E toggle row enabled, ^D delete row", "dans les tableaux : ^E activer/désactiver la ligne, ^D supprimer la ligne", "i guidens tabeller: ^E slå række til/fra, ^D slet række";
    help_copy_selection => "copy the selection, or the whole panel if nothing is selected (Request JSON / Request Hurl / Response panel)", "copier la sélection, ou tout le panneau si rien n'est sélectionné (panneau JSON de requête / Hurl de requête / réponse)", "kopiér markeringen, eller hele ruden hvis intet er markeret (Request JSON / Request Hurl / Response-rude)";
    help_compact => "toggle Response compact view (copy still yields the full body)", "basculer l'aperçu compact de la réponse (la copie donne le corps complet)", "slå Response-kompaktvisning til/fra (kopiering giver hele brødteksten)";
    help_multi_select => "Alt+Click+Drag adds another selection region (plain click clears all)", "Alt+Clic+Glisser ajoute une autre zone de sélection (un clic simple efface tout)", "Alt+Klik+Træk tilføjer endnu et markeringsområde (almindeligt klik rydder alt)";
    help_save_editor => "save a multi-line editor", "enregistrer un éditeur multi-lignes", "gem en flerlinjet editor";
    help_cancel => "close menu / cancel edit", "fermer le menu / annuler la modification", "luk menu / annuller redigering";
    help_quit => "quit", "quitter", "afslut";
    help_group_navigation => "Navigation", "Navigation", "Navigation";
    help_group_tabs => "Tabs", "Onglets", "Faner";
    help_group_requests => "Requests & Running", "Requêtes et exécution", "Anmodninger og kørsel";
    help_group_menus => "Menus & Workspace", "Menus et Workspace", "Menuer og Workspace";
    help_group_environments => "Environments", "Environnements", "Miljøer";
    help_group_editing => "Editing & Selection", "Édition et sélection", "Redigering og markering";
    help_group_panels => "Panels & General", "Panneaux et général", "Paneler og generelt";
    help_group_reports => "Reports", "Rapports", "Rapporter";
    help_report_new => "new report tab (from any view)", "nouvel onglet de rapport (depuis n'importe quelle vue)", "ny rapportfane (fra enhver visning)";
    help_report_edit => "edit the raw report source (type directly in the panel)", "modifier la source brute du rapport (saisir directement dans le panneau)", "rediger den rå rapportkilde (skriv direkte i panelet)";
    help_report_leave_edit => "leave edit focus (single letters are shortcuts again)", "quitter le mode édition (les lettres redeviennent des raccourcis)", "forlad redigering (enkelte bogstaver er genveje igen)";
    help_report_word_move => "move the cursor a word at a time (while editing)", "déplacer le curseur d'un mot à la fois (en édition)", "flyt markøren et ord ad gangen (under redigering)";
    help_report_complete => "fill in the suggested request name (while editing)", "compléter le nom de requête suggéré (en édition)", "udfyld det foreslåede anmodningsnavn (under redigering)";
    help_report_run => "run the report and show its results grid", "exécuter le rapport et afficher sa grille de résultats", "kør rapporten og vis resultatgitteret";
    help_report_dry_run => "preview the projected rows without sending any requests", "prévisualiser les lignes prévues sans envoyer de requêtes", "forhåndsvis de forventede rækker uden at sende anmodninger";
    help_report_view => "toggle between the source and the results grid", "basculer entre la source et la grille de résultats", "skift mellem kilden og resultatgitteret";
    help_report_nodes => "open the structured node editor (Esc returns to the source)", "ouvrir l'éditeur de nœuds structuré (Échap revient à la source)", "åbn den strukturerede node-editor (Esc vender tilbage til kilden)";
    help_report_nodes_edit => "in the node editor: a add · Enter configure · e edit line · f File menu · Del remove · Shift+↑/↓ move · Ctrl+Z undo", "dans l'éditeur de nœuds : a ajouter · Entrée configurer · e modifier la ligne · f menu Fichier · Suppr retirer · Maj+↑/↓ déplacer · Ctrl+Z annuler", "i node-editoren: a tilføj · Enter konfigurer · e rediger linje · f Fil-menu · Del fjern · Skift+↑/↓ flyt · Ctrl+Z fortryd";
    help_report_nodes_forms => "every block has a form: requests carry SHOW / HIDE / WITH (with STATISTICS), loops carry PARALLEL(n), and a compare loop carries the baseline's SHOW", "chaque bloc a un formulaire : les requêtes portent SHOW / HIDE / WITH (avec STATISTICS), les boucles portent PARALLEL(n), et une boucle de comparaison porte le SHOW de la référence", "hver blok har en formular: forespørgsler bærer SHOW / HIDE / WITH (med STATISTICS), løkker bærer PARALLEL(n), og en sammenligningsløkke bærer referencens SHOW";
    help_report_focus_cycle => "cycle focus: editor → results → editor (tree included for workspace reports)", "faire défiler le focus : éditeur → résultats → éditeur (arbre inclus pour les rapports d'espace de travail)", "skift fokus: editor → resultater → editor (træ inkluderet for workspace-rapporter)";
    help_report_workspace_tree => "in a workspace report: ↑↓ move the pinned tree · Enter open a report/collection · ←/→ up/into a folder", "dans un rapport d'espace de travail : ↑↓ déplacer l'arbre épinglé · Entrée ouvrir un rapport/une collection · ←/→ monter/entrer dans un dossier", "i en arbejdsområderapport: ↑↓ flyt det fastgjorte træ · Enter åbn en rapport/samling · ←/→ op/ind i en mappe";
    help_report_export => "export the last run (CSV / JSON / HTML / xlsx by extension)", "exporter la dernière exécution (CSV / JSON / HTML / xlsx selon l'extension)", "eksportér den seneste kørsel (CSV / JSON / HTML / xlsx efter filtype)";
    help_report_baseline => "save the last run as a .baseline snapshot — reference it later with BASELINE(FILE) or COMPARISON(FILE)", "enregistrer la dernière exécution comme instantané .baseline — à réutiliser via BASELINE(FILE) ou COMPARISON(FILE)", "gem den seneste kørsel som et .baseline-øjebliksbillede — genbrug det med BASELINE(FILE) eller COMPARISON(FILE)";
    help_report_columns => "pick, reorder and rename the report's output columns", "choisir, réordonner et renommer les colonnes de sortie du rapport", "vælg, omorden og omdøb rapportens outputkolonner";
    help_report_bind => "bind the report to one of the open collections", "lier le rapport à l'une des collections ouvertes", "bind rapporten til en af de åbne samlinger";
    help_tab_reports => "Reports", "Rapports", "Rapporter";
    help_reports_about_heading => "What is a report?", "Qu'est-ce qu'un rapport ?", "Hvad er en rapport?";
    help_reports_about_1 => "A report is a saved flow that drives a bound collection against ranges of files or environments and collects the results into a table.", "Un rapport est un flux enregistré qui exécute une collection liée sur des ensembles de fichiers ou d'environnements et rassemble les résultats dans un tableau.", "En rapport er et gemt flow, der kører en bundet samling mod intervaller af filer eller miljøer og samler resultaterne i en tabel.";
    help_reports_about_2 => "A programmer writes the flow once; someone with little technical skill then runs it. A report is bound to a collection by name and can be re-pointed at another.", "Un programmeur écrit le flux une fois ; une personne peu technique l'exécute ensuite. Un rapport est lié à une collection par son nom et peut être redirigé vers une autre.", "En programmør skriver flowet én gang; en person med få tekniske færdigheder kører det derefter. En rapport er bundet til en samling ved navn og kan pege på en anden.";
    help_reports_shortcuts_heading => "Report shortcuts", "Raccourcis de rapport", "Rapportgenveje";
    help_reports_grammar_heading => "Flow language", "Langage de flux", "Flow-sprog";
    help_reports_loops_heading => "Loops & producers", "Boucles et sources", "Løkker og kilder";
    help_grammar_collection => "bind the report to a collection (required)", "lier le rapport à une collection (obligatoire)", "bind rapporten til en samling (påkrævet)";
    help_grammar_name => "report name; {time} stamps written files (YYYY-MM-DD-HHMMSS)", "nom du rapport ; {time} horodate les fichiers écrits (AAAA-MM-JJ-HHMMSS)", "rapportnavn; {time} tidsstempler skrevne filer (ÅÅÅÅ-MM-DD-TTMMSS)";
    help_grammar_environment => "run against one loaded environment (optional; no comparison)", "exécuter avec un environnement chargé (optionnel ; sans comparaison)", "kør mod ét indlæst miljø (valgfrit; ingen sammenligning)";
    help_grammar_assign => "set a variable, used elsewhere as {{KEY}}", "définir une variable, utilisée ailleurs comme {{KEY}}", "sæt en variabel, brugt andre steder som {{KEY}}";
    help_grammar_request => "send a request by name (no output row)", "envoyer une requête par son nom (sans ligne de sortie)", "send en anmodning ved navn (ingen outputrække)";
    help_grammar_report => "send a request and add its fields as columns", "envoyer une requête et ajouter ses champs comme colonnes", "send en anmodning og tilføj dens felter som kolonner";
    help_grammar_show => "keep only these response fields (drop a heavy Response)", "ne garder que ces champs de réponse (retirer une réponse volumineuse)", "behold kun disse svarfelter (drop et tungt Response)";
    help_grammar_hide => "drop these response fields (applied last)", "retirer ces champs de réponse (appliqué en dernier)", "fjern disse svarfelter (anvendes sidst)";
    help_grammar_statistics => "summary footer for a column: MEAN/MEDIAN/… or DISTRIBUTION", "pied de résumé d'une colonne : MEAN/MEDIAN/… ou DISTRIBUTION", "opsummeringsfod for en kolonne: MEAN/MEDIAN/… eller DISTRIBUTION";
    help_grammar_image => "draw the column's value (URL, path, base64) as a picture; HEIGHT/WIDTH/FIT size it", "dessine la valeur de la colonne (URL, chemin, base64) comme une image ; HEIGHT/WIDTH/FIT la dimensionnent", "tegn kolonnens værdi (URL, sti, base64) som et billede; HEIGHT/WIDTH/FIT angiver størrelsen";
    help_grammar_with => "field may alias an intrinsic, add STATISTICS, or be quoted", "un champ peut aliaser un intrinsèque, STATISTICS, ou être cité", "et felt kan aliasere en intrinsic, STATISTICS eller citeres";
    help_grammar_parallel => "prefix a FOR to run its iterations concurrently", "préfixer un FOR pour exécuter ses itérations en parallèle", "sæt foran et FOR for at køre dets gentagelser samtidigt";
    help_grammar_for => "loop over a source, binding VAR each pass; END closes it", "boucler sur une source, en liant VAR à chaque passage ; END la ferme", "gennemløb en kilde og bind VAR hver gang; END lukker den";
    help_grammar_for_tuple => "destructure each tuple into several variables", "décomposer chaque tuple en plusieurs variables", "udpak hver tuple i flere variabler";
    help_grammar_pattern => "'_' skips a position; '...' absorbs the rest", "« _ » ignore une position ; « ... » absorbe le reste", "« _ » springer en position over; « ... » opsamler resten";
    help_grammar_list => "name a source so a loop can reuse it below", "nommer une source pour qu'une boucle la réutilise", "navngiv en kilde, så en løkke kan genbruge den nedenfor";
    help_grammar_list_literal => "an inline list of scalars or (\"a\", \"b\") tuples", "une liste littérale de scalaires ou de tuples (\"a\", \"b\")", "en inline-liste af skalarer eller (\"a\", \"b\")-tupler";
    help_grammar_files => "file paths under a folder (glob *, ** — not regex)", "chemins de fichiers dans un dossier (glob *, ** — pas regex)", "filstier i en mappe (glob *, ** — ikke regex)";
    help_grammar_folders => "subfolders (glob filters names, ** recurses); each role-glob binds one file per folder, r=\"g\"? optional", "sous-dossiers (le glob filtre les noms, ** descend) ; chaque glob de rôle lie un fichier par dossier, r=\"g\"? facultatif", "undermapper (glob filtrerer navne, ** går i dybden); hvert rolle-glob binder én fil per mappe, r=\"g\"? valgfri";
    help_grammar_tuples => "rows from a .csv/.tsv/.json file (headers name fields)", "lignes d'un fichier .csv/.tsv/.json (les en-têtes nomment les champs)", "rækker fra en .csv/.tsv/.json-fil (overskrifter navngiver felter)";
    help_grammar_zip => "pair sources positionally (must be equal length)", "apparier les sources par position (longueurs égales)", "par kilder positionelt (skal have samme længde)";
    help_grammar_concat => "append sources end-to-end (same arity)", "concaténer les sources bout à bout (même arité)", "sammenkæd kilder efter hinanden (samme aritet)";
    help_grammar_envs => "loop over environments (BASELINE/COMPARISON to diff)", "boucler sur des environnements (BASELINE/COMPARISON pour comparer)", "gennemløb miljøer (BASELINE/COMPARISON for at sammenligne)";
    help_grammar_baseline_file => "use a saved .baseline snapshot as a role instead of a live env", "utiliser un instantané .baseline enregistré comme rôle au lieu d'un environnement", "brug et gemt .baseline-øjebliksbillede som rolle i stedet for et live-miljø";
    help_grammar_result => "diff column: candidate vs baseline env, per reported field", "colonne de différence : candidat vs référence, par champ rapporté", "forskelskolonne: kandidat vs. reference, pr. rapporteret felt";
    new_request_hint => "Tab/arrows move · PgUp/PgDn tab · Alt+1-9 jump · ^Enter/F2 create · Esc cancel", "Tab/flèches se déplacer · PgUp/PgDn onglet · Alt+1-9 aller à · ^Entrée/F2 créer · Échap annuler", "Tab/pile flyt · PgUp/PgDn faneblad · Alt+1-9 hop til · ^Enter/F2 opret · Esc annuller";
    edit_request_hint => "Tab/arrows move · PgUp/PgDn tab · Alt+1-9 jump · ^Enter/F2 save · Esc cancel", "Tab/flèches se déplacer · PgUp/PgDn onglet · Alt+1-9 aller à · ^Entrée/F2 enregistrer · Échap annuler", "Tab/pile flyt · PgUp/PgDn faneblad · Alt+1-9 hop til · ^Enter/F2 gem · Esc annuller";
    raw_mode_hint => "Edit the raw Hurl text · F2/^Enter reparse & save · Esc cancel · Shift+Arrow select · ^Y copy", "Modifiez le texte Hurl brut · F2/^Entrée réanalyser et enregistrer · Échap annuler · Maj+Flèche sélection · ^Y copier", "Rediger den rå Hurl-tekst · F2/^Enter genfortolk & gem · Esc annuller · Shift+Pil markér · ^Y kopiér";
    raw_json_hint => "Edit the raw JSON · F2/^Enter reparse & save · Esc cancel · Shift+Arrow select · ^Y copy", "Modifiez le JSON brut · F2/^Entrée réanalyser et enregistrer · Échap annuler · Maj+Flèche sélection · ^Y copier", "Rediger den rå JSON · F2/^Enter genfortolk & gem · Esc annuller · Shift+Pil markér · ^Y kopiér";
    ctrl_enter_key => "^Enter", "^Entrée", "^Enter";
    prompt_rename_title => "Rename tab", "Renommer l'onglet", "Omdøb fane";
    prompt_enter_path => "enter path", "saisir le chemin", "indtast sti";
    prompt_save_hint_ml => "F2 save · Esc cancel", "F2 enregistrer · Échap annuler", "F2 gem · Esc annuller";
    prompt_save_hint_sl => "Enter save · Esc cancel", "Entrée enregistrer · Échap annuler", "Enter gem · Esc annuller";
    prompt_reset_hint => "^R reset", "^R réinitialiser", "^R nulstil";
    browser_select_file => "Select file", "Sélectionner un fichier", "Vælg fil";
    browser_hint => "Enter open · type to filter · ← parent · ^h hidden · ^r reset · Esc cancel", "Entrée ouvrir · taper pour filtrer · ← dossier parent · ^h fichiers cachés · ^r réinitialiser · Échap annuler", "Enter åbn · skriv for at filtrere · ← overordnet · ^h skjulte · ^r nulstil · Esc annuller";
    browser_hint_filter => "Enter open · Tab all/matching · type to filter · ← parent · ^h hidden · ^r reset · Esc cancel", "Entrée ouvrir · Tab tous/correspondants · taper pour filtrer · ← dossier parent · ^h fichiers cachés · ^r réinitialiser · Échap annuler", "Enter åbn · Tab alle/matchende · skriv for at filtrere · ← overordnet · ^h skjulte · ^r nulstil · Esc annuller";
    tabs_heading => "Collections", "Collections", "Samlinger";
    suggest_hint => "↓↑ select · Enter fill", "↓↑ sélectionner · Entrée remplir", "↓↑ vælg · Enter udfyld";
    report_default_name => "Untitled Report", "Rapport sans titre", "Unavngiven rapport";
    // The GUI numbers its scratch reports so several can be told apart in the
    // list; `{n}` is the report's position.
    gui_new_report_name => "Report {n}", "Rapport {n}", "Rapport {n}";
    status_not_report => "The active tab is not a report.", "L'onglet actif n'est pas un rapport.", "Den aktive fane er ikke en rapport.";
    report_bound_status => "Report bound to", "Rapport lié à", "Rapport bundet til";
    report_source_heading => "Report Source", "Source du rapport", "Rapportkilde";
    report_validation_heading => "Validation", "Validation", "Validering";
    report_binding_heading => "Binding", "Liaison", "Binding";
    report_bound_prefix => "Bound collection:", "Collection liée :", "Bundet samling:";
    report_env_prefix => "Environment:", "Environnement :", "Miljø:";
    report_env_not_loaded => "(not loaded)", "(non chargé)", "(ikke indlæst)";
    report_base_dir_prefix => "Files resolve under:", "Les fichiers se résolvent sous :", "Filer opløses under:";
    report_base_dir_unsaved => "(save the report to anchor relative paths)", "(enregistrez le rapport pour ancrer les chemins relatifs)", "(gem rapporten for at forankre relative stier)";
    report_unbound => "No collection bound — edit the report's '# collection:' header to bind one.", "Aucune collection liée — modifiez l'en-tête « # collection: » du rapport pour en lier une.", "Ingen samling bundet — rediger rapportens « # collection: »-header for at binde en.";
    report_collection_missing => "Bound collection is not loaded — open it as a tab so its requests can be validated.", "La collection liée n'est pas chargée — ouvrez-la dans un onglet pour valider ses requêtes.", "Den bundne samling er ikke indlæst — åbn den som en fane, så dens anmodninger kan valideres.";
    report_no_diagnostics => "No problems found.", "Aucun problème détecté.", "Ingen problemer fundet.";
    report_empty_source => "Empty report — press e to edit its source.", "Rapport vide — appuyez sur e pour modifier sa source.", "Tom rapport — tryk e for at redigere kilden.";
    report_hint_edit => "e source", "e source", "e kilde";
    report_hint_run => "r run", "r exécuter", "r kør";
    report_hint_leave => "Esc done", "Échap terminé", "Esc færdig";
    report_dirty_marker => "●", "●", "●";
    report_results_heading => "Results", "Résultats", "Resultater";
    report_hint_results => "Enter drill-down · / filter · v source · Ctrl+S export · Ctrl+O open · B baseline", "Entrée exploration · / filtre · v source · Ctrl+S export · Ctrl+O ouvrir · B référence", "Enter udforsk · / filter · v kilde · Ctrl+S export · Ctrl+O åbn · B basislinje";
    report_results_empty => "No results yet — press r to run the report.", "Aucun résultat — appuyez sur r pour exécuter le rapport.", "Ingen resultater endnu — tryk r for at køre rapporten.";
    report_run_parse_error => "Can't run — the source has a parse error:", "Exécution impossible — la source a une erreur d'analyse :", "Kan ikke køre — kilden har en parsefejl:";
    report_run_unbound => "Bind a collection before running (edit the '# collection:' header).", "Liez une collection avant l'exécution (modifiez l'en-tête « # collection: »).", "Bind en samling før kørsel (rediger « # collection: »-headeren).";
    report_run_has_errors => "Fix the validation errors before running.", "Corrigez les erreurs de validation avant l'exécution.", "Ret valideringsfejlene før kørsel.";
    report_export_no_result => "Run the report before exporting.", "Exécutez le rapport avant l'exportation.", "Kør rapporten før eksport.";
    report_export_csv_folder => "Export Report — Choose Destination Folder", "Exporter le rapport — Choisir le dossier de destination", "Eksportér rapport — Vælg destinationsmappe";
    report_export_format_hint => "Format (↑↓)", "Format (↑↓)", "Format (↑↓)";
    report_save_baseline_folder => "Save Report Baseline — Choose Destination Folder", "Enregistrer la référence du rapport — Choisir le dossier de destination", "Gem rapport-basislinje — Vælg destinationsmappe";
    report_run_complete => "Report run complete:", "Exécution du rapport terminée :", "Rapportkørsel fuldført:";
    report_status_rows => "rows", "lignes", "rækker";
    report_status_errors => "errors", "erreurs", "fejl";
    report_exported_prefix => "Report exported to", "Rapport exporté vers", "Rapport eksporteret til";
    report_opened_prefix => "Opened", "Ouvert", "Åbnede";
    // Ctrl+O on a run that hasn't been written anywhere: there is no file for
    // the desktop to open, and saying so is more use than doing nothing.
    report_open_no_export => "Export the run first (Ctrl+S), then Ctrl+O opens the file.", "Exportez d'abord l'exécution (Ctrl+S), puis Ctrl+O ouvre le fichier.", "Eksportér kørslen først (Ctrl+S); derefter åbner Ctrl+O filen.";
    help_report_open_export => "open the exported file in the desktop's default application", "ouvrir le fichier exporté dans l'application par défaut du bureau", "åbn den eksporterede fil i skrivebordets standardprogram";
    report_baseline_saved_prefix => "Baseline saved to", "Référence enregistrée dans", "Basislinje gemt i";
    // "Nothing to save" is true but unhelpful for an export: the reader knows
    // there is a report, so the message has to say what is missing. A run in
    // flight is a different problem from a run never started -- one is "wait",
    // the other is "press Run" (`report_export_no_result`, above).
    report_export_still_running => "The report is still running. Wait for it to finish, then export.", "Le rapport est toujours en cours d'exécution. Attendez la fin, puis exportez.", "Rapporten kører stadig. Vent, til den er færdig, og eksportér så.";
    report_baseline_no_result => "Run the report before saving a baseline.", "Exécutez le rapport avant d'enregistrer une référence.", "Kør rapporten før du gemmer en basislinje.";
    report_hint_dry => "d dry-run", "d simulation", "d prøvekørsel";
    report_hint_bind => "b bind", "b lier", "b bind";
    report_hint_nodes => "Enter nodes", "Entrée nœuds", "Enter noder";
    report_hint_format => "F reindent", "F réindenter", "F genindryk";
    report_hint_view => "v output", "v sortie", "v output";
    gui_report_dry_run => "Dry run", "Simulation", "Prøvekørsel";
    gui_report_dry_run_tooltip => "Preview the rows this report would produce — loops expanded and variables resolved, but no requests sent.", "Prévisualiser les lignes que ce rapport produirait — boucles développées et variables résolues, mais aucune requête envoyée.", "Forhåndsvis de rækker denne rapport ville producere — løkker udvidet og variabler løst, men ingen anmodninger sendt.";
    gui_report_dry_run_close => "Close preview", "Fermer l'aperçu", "Luk forhåndsvisning";
    gui_report_dry_run_close_tooltip => "Go back to the results of the last real run.", "Revenir aux résultats de la dernière exécution réelle.", "Gå tilbage til resultaterne af den seneste rigtige kørsel.";
    report_dry_run_title => "Dry run — expansion preview", "Simulation — aperçu de l'expansion", "Prøvekørsel — udvidelsesforhåndsvisning";
    report_dry_run_rows => "Projected rows:", "Lignes prévues :", "Forventede rækker:";
    report_dry_run_no_rows => "No rows would be produced.", "Aucune ligne ne serait produite.", "Ingen rækker ville blive produceret.";
    report_dry_run_problems_heading => "Problems", "Problèmes", "Problemer";
    report_dry_run_no_problems => "No problems found.", "Aucun problème détecté.", "Ingen problemer fundet.";
    report_dry_run_hint => "↑/↓ scroll · ←/→ columns · Esc close", "↑/↓ défiler · ←/→ colonnes · Échap fermer", "↑/↓ rul · ←/→ kolonner · Esc luk";
    report_dry_run_preview_notice => "Dry run — loop bindings resolved; HTTP response fields blank", "Simulation — liaisons de boucle résolues ; champs de réponse HTTP vides", "Prøvekørsel — løkkebindinger løst; HTTP-svarfelter tomme";
    report_dry_run_warnings_heading => "Warnings", "Avertissements", "Advarsler";
    report_cell_popup_hint => "↑/↓ scroll · y copy · Esc close", "↑/↓ défiler · y copier · Échap fermer", "↑/↓ rul · y kopier · Esc luk";
    report_columns_title => "Columns", "Colonnes", "Kolonner";
    report_columns_hint => "Space toggle · Shift+↑/↓ move · Enter apply · Esc cancel", "Espace bascule · Maj+↑/↓ déplacer · Entrée appliquer · Échap annuler", "Mellemrum skift · Skift+↑/↓ flyt · Enter anvend · Esc annuller";
    report_columns_need_run => "Run the report first so its columns are known", "Exécutez d'abord le rapport pour connaître ses colonnes", "Kør rapporten først, så dens kolonner kendes";
    report_columns_none_selected => "Select at least one column", "Sélectionnez au moins une colonne", "Vælg mindst én kolonne";
    report_columns_applied => "Columns updated", "Colonnes mises à jour", "Kolonner opdateret";
    report_bind_title => "Bind Collection", "Lier la collection", "Bind samling";
    report_bind_hint => "↑/↓ select · Enter bind · Esc cancel", "↑/↓ sélectionner · Entrée lier · Échap annuler", "↑/↓ vælg · Enter bind · Esc annuller";
    report_bind_unsaved => "(unsaved)", "(non enregistré)", "(ikke gemt)";
    report_bind_no_collections => "Open a collection tab first, then bind the report to it", "Ouvrez d'abord un onglet de collection, puis liez-y le rapport", "Åbn først en samlingsfane, og bind derefter rapporten til den";
    report_running => "Running report… (r to cancel)", "Exécution du rapport… (r pour annuler)", "Kører rapport… (r for at annullere)";
    report_running_progress => "Running report… {done}/{total} (r to cancel)", "Exécution du rapport… {done}/{total} (r pour annuler)", "Kører rapport… {done}/{total} (r for at annullere)";
    report_run_stopped => "Run stopped — partial results kept", "Exécution interrompue — résultats partiels conservés", "Kørsel stoppet — delvise resultater beholdt";
    status_request_reverted => "request reverted to last saved", "requête rétablie à la dernière sauvegarde", "anmodning gendannet til sidst gemte";
    status_file_reverted => "file reverted to last saved:", "fichier rétabli à la dernière sauvegarde :", "fil gendannet til sidst gemte:";
    status_env_reverted => "reverted to last saved:", "rétabli à la dernière sauvegarde :", "gendannet til sidst gemte:";
    status_nothing_to_revert => "Nothing to revert (no saved version or no changes)", "Rien à rétablir (aucune version sauvegardée ou aucune modification)", "Intet at gendanne (ingen gemt version eller ingen ændringer)";
    report_running_indicator => "⏳ Running…", "⏳ En cours…", "⏳ Kører…";
    report_nodes_heading => "Structure", "Structure", "Struktur";
    report_nodes_hint => "a add · Enter configure · e edit line · f File · Del remove · Shift+↑/↓ move · Ctrl+Z undo · Esc source", "a ajouter · Entrée configurer · e modifier la ligne · f Fichier · Suppr retirer · Maj+↑/↓ déplacer · Ctrl+Z annuler · Échap source", "a tilføj · Enter konfigurer · e rediger linje · f Fil · Del fjern · Skift+↑/↓ flyt · Ctrl+Z fortryd · Esc kilde";
    report_nodes_parse_error => "Fix the source before editing as nodes", "Corrigez la source avant de modifier en nœuds", "Ret kilden før redigering som noder";
    report_node_begin => "Begin", "Début", "Start";
    report_node_end => "End", "Fin", "Slut";
    node_menu_title => "Add Node", "Ajouter un nœud", "Tilføj node";
    node_menu_hint => "↑/↓ select · Enter add · Esc cancel", "↑/↓ sélectionner · Entrée ajouter · Échap annuler", "↑/↓ vælg · Enter tilføj · Esc annuller";
    node_pick_request_title => "Choose Request", "Choisir une requête", "Vælg forespørgsel";
    node_pick_request_hint => "↑/↓ select · Enter choose · Esc cancel", "↑/↓ sélectionner · Entrée choisir · Échap annuler", "↑/↓ vælg · Enter vælg · Esc annuller";
    node_pick_request_none => "No requests in the bound collection", "Aucune requête dans la collection liée", "Ingen forespørgsler i den bundne samling";
    node_kind_request => "REQUEST — send a request", "REQUEST — envoyer une requête", "REQUEST — send en forespørgsel";
    node_kind_report_request => "REPORT REQUEST — send and report its fields", "REPORT REQUEST — envoyer et rapporter ses champs", "REPORT REQUEST — send og rapportér dens felter";
    node_kind_report_var => "REPORT — report a variable", "REPORT — rapporter une variable", "REPORT — rapportér en variabel";
    node_kind_report_computed => "REPORT \"…\" AS — a computed column", "REPORT \"…\" AS — une colonne calculée", "REPORT \"…\" AS — en beregnet kolonne";
    node_kind_assign => "VARIABLE — set VAR = VALUE", "VARIABLE — définir VAR = VALEUR", "VARIABLE — sæt VAR = VÆRDI";
    node_kind_for_files => "FOR … IN FILES — loop over files", "FOR … IN FILES — boucler sur des fichiers", "FOR … IN FILES — løkke over filer";
    node_kind_for_folders => "FOR … IN FOLDERS — loop over folders", "FOR … IN FOLDERS — boucler sur des dossiers", "FOR … IN FOLDERS — løkke over mapper";
    node_kind_for_envs => "FOR … IN ENVS — loop over environments", "FOR … IN ENVS — boucler sur des environnements", "FOR … IN ENVS — løkke over miljøer";
    node_kind_list => "LIST — declare a list", "LIST — déclarer une liste", "LIST — erklær en liste";
    node_mod_report => "REPORT — report a request", "REPORT — rapporter une requête", "REPORT — rapportér en forespørgsel";
    node_mod_parallel => "PARALLEL — run a loop concurrently", "PARALLEL — exécuter une boucle en parallèle", "PARALLEL — kør en løkke samtidigt";
    node_mod_with => "WITH — add a field to a report request", "WITH — ajouter un champ à une requête de rapport", "WITH — tilføj et felt til en rapportforespørgsel";
    node_mod_as => "AS — name a report column", "AS — nommer une colonne du rapport", "AS — navngiv en rapportkolonne";
    node_mod_response => "RESPONSE — capture the response body (RAW/PRETTY)", "RESPONSE — capturer le corps de la réponse (RAW/PRETTY)", "RESPONSE — indfang svarets krop (RAW/PRETTY)";
    node_mod_show => "SHOW — pick which fields to show", "SHOW — choisir les champs à afficher", "SHOW — vælg hvilke felter der vises";
    node_mod_statistics => "STATISTICS", "STATISTICS", "STATISTICS";
    node_mod_hide => "HIDE — pick which fields to hide", "HIDE — choisir les champs à masquer", "HIDE — vælg hvilke felter der skjules";
    // Why a modifier chip refuses to attach to the block under the pointer.
    // Shown as a tooltip during the drag, so the drop never fails in silence.
    mod_reject_present => "This block already has that.", "Ce bloc l'a déjà.", "Denne blok har allerede det.";
    mod_reject_report => "REPORT can only be added to a request or a SET assignment.", "REPORT ne peut être ajouté qu'à une requête ou à une affectation SET.", "REPORT kan kun tilføjes til en forespørgsel eller en SET-tildeling.";
    mod_reject_parallel => "PARALLEL can only be added to a FOR loop.", "PARALLEL ne peut être ajouté qu'à une boucle FOR.", "PARALLEL kan kun tilføjes til en FOR-løkke.";
    mod_reject_with => "WITH can only be added to a reported request (add REPORT first).", "WITH ne peut être ajouté qu'à une requête rapportée (ajoutez d'abord REPORT).", "WITH kan kun tilføjes til en rapporteret forespørgsel (tilføj REPORT først).";
    mod_reject_as => "AS can only name a reported request or a single reported variable.", "AS ne peut nommer qu'une requête rapportée ou une seule variable rapportée.", "AS kan kun navngive en rapporteret forespørgsel eller én rapporteret variabel.";
    mod_reject_with_field => "A WITH field only takes STATISTICS — its other clauses are set in the field editor.", "Un champ WITH n'accepte que STATISTICS — ses autres clauses se règlent dans l'éditeur de champ.", "Et WITH-felt tager kun STATISTICS — dets øvrige klausuler sættes i felteditoren.";
    mod_reject_statistics => "STATISTICS summarises a named column — drop it on a REPORT … AS … or a computed column", "STATISTICS résume une colonne nommée — déposez-le sur un REPORT … AS … ou une colonne calculée", "STATISTICS opsummerer en navngivet kolonne — slip den på et REPORT … AS … eller en beregnet kolonne";
    mod_reject_request_only => "This can only be added to a reported request (add REPORT first).", "Ceci ne peut être ajouté qu'à une requête rapportée (ajoutez d'abord REPORT).", "Dette kan kun tilføjes til en rapporteret forespørgsel (tilføj REPORT først).";
    mod_reject_compare_only => "This can only be added to a FOR … IN ENVS comparison loop.", "Ceci ne peut être ajouté qu'à une boucle de comparaison FOR … IN ENVS.", "Dette kan kun tilføjes til en FOR … IN ENVS-sammenligningsløkke.";
    report_node_edit_title => "Edit Node Line", "Modifier la ligne du nœud", "Rediger nodelinje";
    report_node_edit_hint => "Enter apply · Esc cancel", "Entrée appliquer · Échap annuler", "Enter anvend · Esc annuller";
    report_node_line_invalid => "Not a valid statement", "Instruction non valide", "Ikke en gyldig sætning";
    report_node_undone => "Undid last node change", "Dernière modification de nœud annulée", "Fortrød sidste nodeændring";
    report_node_undo_empty => "Nothing to undo", "Rien à annuler", "Intet at fortryde";
    report_node_folder_pick => "Choose loop folder", "Choisir le dossier de la boucle", "Vælg løkkemappe";
    report_header_root_pick => "Choose report root folder", "Choisir le dossier racine du rapport", "Vælg rapportens rodmappe";
    report_header_baseline_pick => "Choose baseline file", "Choisir le fichier de référence", "Vælg baseline-fil";
    gui_report_reindent => "Reindent", "Réindenter", "Genindryk";
    gui_report_reindent_help => "Re-indent every line to its block depth. Only whitespace changes — comments and blank lines are kept.", "Réindenter chaque ligne selon sa profondeur de bloc. Seuls les espaces changent : les commentaires et les lignes vides sont conservés.", "Genindryk hver linje til dens blokdybde. Kun mellemrum ændres — kommentarer og tomme linjer bevares.";
    report_reformat_unsafe => "Reformatting would have changed what the report does, so nothing was changed", "Le reformatage aurait modifié le comportement du rapport ; rien n'a été changé", "Omformatering ville have ændret, hvad rapporten gør, så intet blev ændret";
    report_reformatted => "Report reindented", "Rapport réindenté", "Rapport genindrykket";
    report_already_tidy => "The report is already correctly indented", "Le rapport est déjà correctement indenté", "Rapporten er allerede korrekt indrykket";
    report_reformat_failed_prefix => "Can't reindent:", "Réindentation impossible :", "Kan ikke genindrykke:";
    report_with_add_row => "add a field…", "ajouter un champ…", "tilføj et felt…";
    report_settings_heading => "Report Settings", "Réglages du rapport", "Rapportindstillinger";
    report_setting_add_row => "add a setting…", "ajouter un réglage…", "tilføj en indstilling…";
    report_setting_no_choices => "Nothing to choose from — load an environment first, or press e to type a name", "Rien à choisir — chargez d'abord un environnement, ou appuyez sur e pour saisir un nom", "Intet at vælge imellem — indlæs først et miljø, eller tryk e for at skrive et navn";
    report_setting_menu_hint => "↑↓ move · Enter choose · Esc cancel", "↑↓ déplacer · Entrée choisir · Échap annuler", "↑↓ flyt · Enter vælg · Esc annuller";
    report_node_config_title => "Configure node", "Configurer le nœud", "Konfigurer node";
    report_node_request_hint => "↑↓ move · Space/←→ toggle/cycle · type alias · Enter apply · Esc cancel", "↑↓ déplacer · Espace/←→ bascule/défile · saisir l'alias · Entrée appliquer · Échap annuler", "↑↓ flyt · Mellemrum/←→ skift · skriv alias · Enter anvend · Esc annuller";
    report_node_name_label => "Name", "Nom", "Navn";
    report_node_name_none => "pick a request", "choisir une requête", "vælg en forespørgsel";
    report_node_report_label => "Report (emit columns)", "Rapport (émettre des colonnes)", "Rapport (udsend kolonner)";
    report_node_response_label => "Response", "Réponse", "Svar";
    report_node_response_default => "default", "défaut", "standard";
    report_node_alias_label => "Alias", "Alias", "Alias";
    report_node_alias_none => "(request name)", "(nom de la requête)", "(forespørgselsnavn)";
    report_node_envs_title => "Configure ENVS loop", "Configurer la boucle ENVS", "Konfigurer ENVS-løkke";
    report_node_envs_hint => "↑↓ move · ←/→ pick · Space toggle · b baseline · f file · n add · x remove · Enter apply · Esc cancel", "↑↓ déplacer · ←/→ choisir env · b référence · f fichier · n ajouter · x retirer · Entrée appliquer · Échap annuler", "↑↓ flyt · ←/→ vælg miljø · b basislinje · f fil · n tilføj · x fjern · Enter anvend · Esc annuller";
    report_node_envs_var_label => "Loop variable", "Variable de boucle", "Løkkevariabel";
    report_node_envs_mode_label => "Mode", "Mode", "Tilstand";
    report_node_envs_mode_plain => "Iterate", "Itérer", "Iterér";
    report_node_envs_mode_roles => "Compare", "Comparer", "Sammenlign";
    report_node_envs_baseline => "Baseline", "Référence", "Basislinje";
    report_node_envs_comparison => "Comparison", "Comparaison", "Sammenligning";
    report_node_envs_file => "FILE", "FILE", "FILE";
    report_node_envs_none => "no environments loaded — load one to pick", "aucun environnement chargé — en charger un pour choisir", "ingen miljøer indlæst — indlæs et for at vælge";
    report_node_vars_title => "Report a variable", "Rapporter une variable", "Rapportér en variabel";
    report_node_vars_hint => "↑↓ move · Space tick · type a name/alias · Enter apply · Esc cancel", "↑↓ déplacer · Espace cocher · saisir un nom/alias · Entrée appliquer · Échap annuler", "↑↓ flyt · Mellemrum markér · skriv et navn/alias · Enter anvend · Esc annuller";
    report_node_vars_other_label => "Other variable", "Autre variable", "Anden variabel";
    report_node_vars_none => "no variables in scope here — type one below", "aucune variable disponible ici — saisissez-en une ci-dessous", "ingen variabler i omfang her — skriv en nedenfor";
    report_node_computed_title => "Computed column", "Colonne calculée", "Beregnet kolonne";
    report_node_computed_hint => "↑↓ move · type template/name · Space toggle stat · Enter apply · Esc cancel", "↑↓ déplacer · saisir modèle/nom · Espace bascule stat · Entrée appliquer · Échap annuler", "↑↓ flyt · skriv skabelon/navn · Mellemrum skift statistik · Enter anvend · Esc annuller";
    report_node_computed_template_label => "Template", "Modèle", "Skabelon";
    report_node_computed_template_hint => "text with {{ vars }}", "texte avec {{ vars }}", "tekst med {{ vars }}";
    report_node_computed_name_label => "Column name", "Nom de la colonne", "Kolonnenavn";
    report_node_assign_hint => "↑↓ move · type name/value · Enter apply · Esc cancel", "↑↓ déplacer · saisir nom/valeur · Entrée appliquer · Échap annuler", "↑↓ flyt · skriv navn/værdi · Enter anvend · Esc annuller";
    report_node_list_hint => "↑↓ move · type value · Space add · Del remove · Enter apply · Esc cancel", "↑↓ déplacer · saisir la valeur · Espace ajouter · Suppr retirer · Entrée appliquer · Échap annuler", "↑↓ flyt · skriv værdi · Mellemrum tilføj · Del fjern · Enter anvend · Esc annuller";
    report_node_list_add => "+ Add a value", "+ Ajouter une valeur", "+ Tilføj en værdi";
    report_node_with_add => "+ Add a WITH field", "+ Ajouter un champ WITH", "+ Tilføj et WITH-felt";
    report_node_with_title => "Configure WITH field", "Configurer le champ WITH", "Konfigurer WITH-felt";
    report_node_with_hint => "↑↓ move · type name/query · Space toggle · Enter apply · Esc cancel", "↑↓ déplacer · saisir nom/requête · Espace basculer · Entrée appliquer · Échap annuler", "↑↓ flyt · skriv navn/forespørgsel · Mellemrum skift · Enter anvend · Esc annullér";
    report_node_with_name_label => "Column name", "Nom de la colonne", "Kolonnenavn";
    report_node_with_query_label => "Query", "Requête", "Forespørgsel";
    report_node_with_stats_label => "STATISTICS", "STATISTICS", "STATISTICS";
    report_node_with_stats_toggle => "Summary statistics", "Statistiques de synthèse", "Opsummerende statistik";
    report_node_with_query_none => "whole response", "réponse entière", "hele svaret";
    report_node_clause_truth_label => "Ground truth", "Vérité terrain", "Grundsandhed";
    report_node_clause_truth_none => "not scored", "non évalué", "ikke vurderet";
    report_node_clause_detail_toggle => "Detail only (drill-down)", "Détail seulement (volet)", "Kun detalje (udfoldning)";
    report_node_clause_image_toggle => "Show as a picture", "Afficher comme image", "Vis som billede";
    report_node_clause_fit => "Fit to the cell", "Ajuster à la cellule", "Tilpas til cellen";
    report_node_clause_height => "Height (px)", "Hauteur (px)", "Højde (px)";
    report_node_clause_width => "Width (px)", "Largeur (px)", "Bredde (px)";
    report_node_clause_size_auto => "auto", "auto", "auto";
    report_node_parallel_degree_label => "Max at once", "Maximum simultané", "Maks. samtidige";
    report_node_parallel_degree_none => "no limit", "sans limite", "ingen grænse";
    report_node_baseline_show_label => "SHOW from baseline", "SHOW depuis la référence", "SHOW fra reference";
    report_node_parallel_label => "Run PARALLEL", "Exécuter en PARALLÈLE", "Kør PARALLELT";
    checkbox_checked => "[x]", "[x]", "[x]";
    checkbox_unchecked => "[ ]", "[ ]", "[ ]";
    report_node_files_title => "Configure FILES loop", "Configurer la boucle FILES", "Konfigurer FILES-løkke";
    report_node_folders_title => "Configure FOLDERS loop", "Configurer la boucle FOLDERS", "Konfigurer FOLDERS-løkke";
    report_node_files_hint => "↑↓ move · Space folder/parallel · type var/match/limit · Enter apply · Esc cancel", "↑↓ déplacer · Espace dossier/parallèle · saisir var/match · Entrée appliquer · Échap annuler", "↑↓ flyt · Mellemrum mappe/parallel · skriv var/match · Enter anvend · Esc annuller";
    report_node_files_var_label => "Loop variable", "Variable de boucle", "Løkkevariabel";
    report_node_files_folder_label => "Folder", "Dossier", "Mappe";
    report_node_files_match_label => "Match (glob)", "Filtre (glob)", "Match (glob)";
    report_node_files_none => "no folder chosen — Space to pick", "aucun dossier choisi — Espace pour choisir", "ingen mappe valgt — Mellemrum for at vælge";

    // ── GUI front-end strings ────────────────────────────────────────────────
    // Clean (mnemonic-free) labels for the graphical client. The terminal UI's
    // rows above embed `(X)` keyboard-mnemonic hints, so the GUI needs its own
    // wording; kept here so all three languages stay in one table.
    gui_untitled => "Untitled", "Sans titre", "Uden titel";
    gui_new_collection => "New collection", "Nouvelle collection", "Ny samling";
    gui_env_label => "Env:", "Env\u{a0}:", "Miljø:";
    gui_theme_status_label => "Theme:", "Thème\u{a0}:", "Tema:";
    gui_none_dash => "—", "—", "—";
    // Editor sections.
    gui_sec_params => "Params", "Paramètres", "Parametre";
    gui_sec_headers => "Headers", "En-têtes", "Headere";
    gui_sec_body => "Body", "Corps", "Body";
    gui_sec_auth => "Auth", "Auth", "Auth";
    gui_sec_cookies => "Cookies", "Cookies", "Cookies";
    gui_sec_options => "Options", "Options", "Indstillinger";
    gui_sec_asserts => "Asserts", "Assertions", "Assertioner";
    gui_sec_captures => "Captures", "Captures", "Optagelser";
    gui_sec_code => "Raw Request", "Requête brute", "Rå anmodning";
    // The two representations the Raw Request view can show. Both are format
    // names rather than words, so they read the same in every language.
    gui_code_repr_json => "JSON", "JSON", "JSON";
    gui_code_repr_hurl => "Hurl", "Hurl", "Hurl";
    gui_code_parse_error => "Invalid request text — edits not applied.", "Texte de requête invalide — modifications non appliquées.", "Ugyldig anmodningstekst — ændringer ikke anvendt.";
    gui_query_parameters => "Query parameters", "Paramètres de requête", "Forespørgselsparametre";
    gui_form_fields => "Form / multipart fields", "Champs de formulaire / multipart", "Formular-/multipart-felter";
    gui_form_mutually_exclusive => "This request uses form fields (below). A raw body and form fields are mutually exclusive.", "Cette requête utilise des champs de formulaire (ci-dessous). Un corps brut et des champs de formulaire sont mutuellement exclusifs.", "Denne anmodning bruger formularfelter (nedenfor). En rå brødtekst og formularfelter udelukker hinanden.";
    gui_raw_body_hint => "Raw request body (JSON, text, …)", "Corps brut de la requête (JSON, texte, …)", "Rå anmodningstekst (JSON, tekst, …)";
    gui_basic_auth => "Basic authentication", "Authentification basique", "Basisgodkendelse";
    gui_username => "Username", "Nom d'utilisateur", "Brugernavn";
    gui_password => "Password", "Mot de passe", "Adgangskode";
    gui_per_request_options => "Per-request Hurl options (e.g. retry: 3, insecure: true)", "Options Hurl par requête (p. ex. retry: 3, insecure: true)", "Hurl-indstillinger pr. anmodning (f.eks. retry: 3, insecure: true)";
    gui_response_assertions => "Response assertions (Hurl expressions)", "Assertions de réponse (expressions Hurl)", "Svar-assertioner (Hurl-udtryk)";
    gui_expected_status => "Expected status", "Statut attendu", "Forventet status";
    gui_captures_help => "Capture values from the response for later requests", "Capturer des valeurs de la réponse pour des requêtes ultérieures", "Fang værdier fra svaret til senere anmodninger";
    gui_add_assert => "+ Add assert", "+ Ajouter une assertion", "+ Tilføj assertion";
    gui_add_field => "+ Add field", "+ Ajouter un champ", "+ Tilføj felt";
    gui_base64_prefix => "base64 prefix", "préfixe base64", "base64-præfiks";
    gui_no_requests_editor => "This collection has no requests.", "Cette collection ne contient aucune requête.", "Denne samling har ingen anmodninger.";
    gui_new_request => "New request", "Nouvelle requête", "Ny anmodning";
    gui_new_request_btn => "New request", "Nouvelle requête", "Ny anmodning";
    gui_send => "Send", "Envoyer", "Send";
    gui_kind_text => "Text", "Texte", "Tekst";
    gui_kind_file => "File", "Fichier", "Fil";
    gui_kind_base64 => "Base64", "Base64", "Base64";
    // Field hints.
    gui_hint_key => "key", "clé", "nøgle";
    gui_hint_value => "value", "valeur", "værdi";
    gui_hint_header => "Header", "En-tête", "Header";
    gui_hint_name => "name", "nom", "navn";
    gui_hint_query => "query", "requête", "forespørgsel";
    gui_hint_description => "note", "note", "note";
    gui_hint_option => "option", "option", "indstilling";
    gui_hint_field => "field", "champ", "felt";
    // Example-shaped hints. The URL and the Hurl assertion are literal syntax
    // (a URL, a Hurl expression), so they stay identical across languages;
    // only the file-path placeholder has words to translate.
    gui_hint_url => "https://api.example.com/path", "https://api.example.com/path", "https://api.example.com/path";
    gui_hint_assert => "jsonpath \"$.status\" == \"ok\"", "jsonpath \"$.status\" == \"ok\"", "jsonpath \"$.status\" == \"ok\"";
    gui_hint_file_path => "/path/to/file", "/chemin/vers/fichier", "/sti/til/fil";
    gui_hint_key_upper => "KEY", "CLÉ", "NØGLE";
    // Response viewer.
    gui_error => "Error", "Erreur", "Fejl";
    gui_no_response_yet => "No response yet", "Aucune réponse", "Intet svar endnu";
    gui_copy => "Copy", "Copier", "Kopiér";
    gui_copy_body => "Copy body", "Copier le corps", "Kopiér brødtekst";
    gui_compact => "Compact", "Compact", "Kompakt";
    gui_compact_hint => "Shorten long string values to a \"head…tail\" overview (copying still yields the full body)", "Raccourcir les longues valeurs de chaîne en un aperçu « début…fin » (la copie renvoie toujours le corps complet)", "Forkort lange strengværdier til et \"start…slut\"-overblik (kopiering giver stadig hele brødteksten)";
    gui_empty_body => "(empty body)", "(corps vide)", "(tom brødtekst)";
    gui_no_headers => "(no headers)", "(aucun en-tête)", "(ingen headere)";
    gui_no_assertions => "No assertions on this request.", "Aucune assertion pour cette requête.", "Ingen assertioner på denne anmodning.";
    // Environments panel.
    gui_environments => "Environments", "Environnements", "Miljøer";
    gui_load_ellipsis => "Load…", "Charger…", "Indlæs…";
    gui_load_vars_tooltip => "Load a .vars file", "Charger un fichier .vars", "Indlæs en .vars-fil";
    gui_new_environment => "New environment", "Nouvel environnement", "Nyt miljø";
    gui_no_environments => "No environments. Load a .vars file or add one.", "Aucun environnement. Chargez un fichier .vars ou ajoutez-en un.", "Ingen miljøer. Indlæs en .vars-fil eller tilføj et.";
    gui_env_filter_hint => "Filter environments…", "Filtrer les environnements…", "Filtrér miljøer…";
    gui_env_filter_no_matches => "No environment matches the filter.", "Aucun environnement ne correspond au filtre.", "Intet miljø matcher filteret.";
    gui_env_source_both => "Both", "Les deux", "Begge";
    gui_env_source_global => "Global", "Global", "Global";
    gui_env_source_workspace => "Workspace", "Workspace", "Workspace";
    gui_env_source_no_matches => "No environments from this source.", "Aucun environnement de cette source.", "Ingen miljøer fra denne kilde.";
    gui_env_open_workspace_tooltip => "In this workspace — click to open and expand it", "Dans cet espace de travail — cliquez pour l'ouvrir et le développer", "I dette arbejdsområde — klik for at åbne og udvide det";
    gui_ws_revert_request => "Revert request to saved", "Rétablir la requête à la sauvegarde", "Gendan anmodning til gemt";
    gui_ws_revert_file => "Revert file to saved", "Rétablir le fichier à la sauvegarde", "Gendan fil til gemt";
    gui_revert_title => "Revert to saved", "Rétablir à la sauvegarde", "Gendan til gemt";
    gui_revert_go => "Revert", "Rétablir", "Gendan";
    gui_ws_set_active_env => "Set as active environment", "Définir comme environnement actif", "Angiv som aktivt miljø";
    gui_active => "Active", "Actif", "Aktiv";
    gui_active_tooltip => "Use this environment for substitution", "Utiliser cet environnement pour la substitution", "Brug dette miljø til substitution";
    gui_linked => "Linked", "Lié", "Tilknyttet";
    gui_linked_tooltip => "Pin to the active collection (overrides Active)", "Épingler à la collection active (remplace Actif)", "Fastgør til den aktive samling (tilsidesætter Aktiv)";
    gui_delete => "Delete", "Supprimer", "Slet";
    gui_save_ellipsis => "Save…", "Enregistrer…", "Gem…";
    gui_add_variable => "+ Add variable", "+ Ajouter une variable", "+ Tilføj variabel";
    gui_resolving => "resolving…", "résolution…", "løser…";
    gui_unresolved => "unresolved", "non résolu", "uløst";
    // Requests tree.
    gui_add_request => "Add request", "Ajouter une requête", "Tilføj anmodning";
    gui_run_all => "Run All", "Tout exécuter", "Kør alle";
    gui_run_all_tooltip => "Run every request in order", "Exécuter toutes les requêtes dans l'ordre", "Kør alle anmodninger i rækkefølge";
    gui_no_requests_tree => "No requests yet — click Add to create one.", "Aucune requête — cliquez sur Ajouter pour en créer une.", "Ingen anmodninger endnu — klik på Tilføj for at oprette en.";
    gui_untitled_request => "(untitled)", "(sans titre)", "(uden titel)";
    gui_run => "Run", "Exécuter", "Kør";
    gui_rename_ellipsis => "Rename…", "Renommer…", "Omdøb…";
    gui_edited_request => "Edited — not saved yet", "Modifiée — pas encore enregistrée", "Ændret — endnu ikke gemt";
    gui_edited_collection => "Has unsaved edits", "Contient des modifications non enregistrées", "Har ugemte ændringer";
    // Shared widgets.
    gui_add => "+ Add", "+ Ajouter", "+ Tilføj";
    gui_remove => "Remove", "Supprimer", "Fjern";
    // Menu bar.
    gui_menu_file => "File", "Fichier", "Fil";
    gui_menu_view => "View", "Affichage", "Vis";
    gui_menu_settings => "Settings", "Paramètres", "Indstillinger";
    // The Alt-key mnemonic for each top-level menu: the letter that opens it
    // after Alt (and the one underlined in its title while Alt is armed).
    //
    // Per language, and *not* derived from the first letter of the title,
    // because the titles are translated and their initials collide: in French,
    // "Fichier" and... nothing else, but in Danish "Fil" and "Vis" are fine
    // while a fourth menu could easily clash. Keeping them in the table means a
    // translator picks a letter that suits their own words (French uses P for
    // "Paramètres", Danish I for "Indstillinger"), and the uniqueness test
    // below catches a collision at build time rather than in the field.
    gui_menu_file_key => "F", "F", "F";
    gui_menu_view_key => "V", "A", "V";
    gui_menu_settings_key => "S", "P", "I";
    gui_new_collection_ellipsis => "New collection…", "Nouvelle collection…", "Ny samling…";
    gui_menu_open => "Open", "Ouvrir", "Åbn";
    gui_menu_kind_response => "Response", "Réponse", "Svar";
    gui_menu_from_file => "From a file…", "Depuis un fichier…", "Fra en fil…";
    gui_menu_from_folder => "From a folder…", "Depuis un dossier…", "Fra en mappe…";
    gui_menu_from_git => "From Git…", "Depuis Git…", "Fra Git…";
    gui_menu_from_postman => "From Postman…", "Depuis Postman…", "Fra Postman…";
    gui_menu_to_file => "To a file…", "Vers un fichier…", "Til en fil…";
    gui_menu_to_git => "To Git…", "Vers Git…", "Til Git…";
    gui_menu_save => "Save", "Enregistrer", "Gem";
    gui_menu_save_as => "Save As…", "Enregistrer sous…", "Gem som…";
    gui_shortcut_save => "Ctrl+S", "Ctrl+S", "Ctrl+S";
    help_menu_save => "Write this straight back to the file it came from", "Réécrire directement dans le fichier d'origine", "Skriv direkte tilbage til filen, den kom fra";
    help_menu_save_unsaved => "This has never been saved, so you will be asked where to put it", "Ceci n'a jamais été enregistré\u{a0}; l'emplacement vous sera demandé", "Dette er aldrig blevet gemt, så du bliver spurgt om hvor";
    gui_set_base_url => "Set base URL…", "Définir l'URL de base…", "Angiv basis-URL…";
    gui_close_tab => "Close tab", "Fermer l'onglet", "Luk fane";
    gui_quit => "Quit", "Quitter", "Afslut";
    gui_language => "Language", "Langue", "Sprog";
    gui_theme_menu => "Theme", "Thème", "Tema";
    gui_follow_language => "Follow language", "Suivre la langue", "Følg sproget";
    gui_new_custom_theme => "New custom theme…", "Nouveau thème personnalisé…", "Nyt brugerdefineret tema…";
    gui_edit_current_theme => "Edit current theme…", "Modifier le thème actuel…", "Rediger nuværende tema…";
    gui_preferences => "Preferences", "Préférences", "Præferencer";
    gui_confirm_on_exit => "Confirm on exit", "Confirmer à la sortie", "Bekræft ved afslutning";
    gui_confirm_on_clear => "Confirm on clear", "Confirmer avant de fermer", "Bekræft ved lukning";
    gui_confirm_delete_env => "Confirm deleting an environment", "Confirmer la suppression d'un environnement", "Bekræft sletning af et miljø";
    gui_run_all_batch => "Run All in batch mode", "Tout exécuter en mode batch", "Kør alle i batch-tilstand";
    gui_default_code_hurl => "Default Code view: Hurl", "Vue Code par défaut\u{a0}: Hurl", "Standard Code-visning: Hurl";
    gui_request_response => "Request / Response", "Requête / Réponse", "Anmodning / Svar";
    gui_reports => "Reports", "Rapports", "Rapporter";
    gui_send_tooltip => "Send the selected request (Ctrl+Enter / F5)", "Envoyer la requête sélectionnée (Ctrl+Entrée / F5)", "Send den valgte anmodning (Ctrl+Enter / F5)";
    gui_custom => "Custom", "Personnalisé", "Brugerdefineret";
    gui_default_env_name => "Environment", "Environnement", "Miljø";
    // Dialogs.
    gui_open_collection_title => "Open collection", "Ouvrir une collection", "Åbn samling";
    gui_open_environment_title => "Open environment", "Ouvrir un environnement", "Åbn miljø";
    gui_filter_collections => "Collections (Hurl / Postman)", "Collections (Hurl / Postman)", "Samlinger (Hurl / Postman)";
    gui_filter_environments => "Environments", "Environnements", "Miljøer";
    gui_filter_all => "All files", "Tous les fichiers", "Alle filer";
    gui_browse => "Browse…", "Parcourir…", "Gennemse…";
    gui_open_workspace_title => "Open workspace folder", "Ouvrir un dossier d'espace de travail", "Åbn workspace-mappe";
    gui_not_a_folder => "That path is not a folder.", "Ce chemin n'est pas un dossier.", "Den sti er ikke en mappe.";
    gui_workspace_filter => "Filter", "Filtre", "Filter";
    gui_ws_new => "New", "Nouveau", "Ny";
    gui_ws_new_tooltip => "Add a collection, report or environment to this workspace", "Ajouter une collection, un rapport ou un environnement à cet espace de travail", "Tilføj en samling, rapport eller et miljø til dette arbejdsområde";
    gui_ws_new_collection => "New collection…", "Nouvelle collection…", "Ny samling…";
    gui_ws_new_report => "New report…", "Nouveau rapport…", "Ny rapport…";
    gui_ws_new_environment => "New environment…", "Nouvel environnement…", "Nyt miljø…";
    gui_ws_new_folder => "New folder…", "Nouveau dossier…", "Ny mappe…";
    gui_ws_new_in_folder => "New in this folder", "Nouveau dans ce dossier", "Ny i denne mappe";
    gui_ws_new_in_root => "New in this workspace", "Nouveau dans cet espace de travail", "Ny i dette arbejdsområde";
    gui_ws_new_collection_title => "Name the new collection", "Nommez la nouvelle collection", "Navngiv den nye samling";
    gui_ws_new_folder_title => "Name the new folder", "Nommez le nouveau dossier", "Navngiv den nye mappe";
    gui_ws_new_report_title => "Name the new report", "Nommez le nouveau rapport", "Navngiv den nye rapport";
    gui_ws_new_environment_title => "Name the new environment", "Nommez le nouvel environnement", "Navngiv det nye miljø";
    gui_workspace_filter_tooltip => "Toggle showing only .hurl/.json/.vars/.trail files", "Basculer l'affichage des seuls fichiers .hurl/.json/.vars/.trail", "Skift visning af kun .hurl/.json/.vars/.trail-filer";
    gui_close => "Close", "Fermer", "Luk";
    gui_cancel => "Cancel", "Annuler", "Annuller";
    gui_could_not_parse => "Could not parse that file.", "Impossible d'analyser ce fichier.", "Kunne ikke fortolke den fil.";
    gui_could_not_read => "Could not read file:", "Impossible de lire le fichier\u{a0}:", "Kunne ikke læse filen:";
    gui_could_not_write => "Could not write file:", "Impossible d'écrire le fichier\u{a0}:", "Kunne ikke skrive filen:";
    gui_open_report_title => "Open report", "Ouvrir un rapport", "Åbn rapport";
    gui_save_report_title => "Save report", "Enregistrer le rapport", "Gem rapport";
    gui_filter_reports => "PaperTrail reports", "Rapports PaperTrail", "PaperTrail-rapporter";
    gui_save_collection_title => "Save collection", "Enregistrer la collection", "Gem samling";
    gui_save_environment_title => "Save environment", "Enregistrer l'environnement", "Gem miljø";
    gui_save_response_title => "Save response", "Enregistrer la réponse", "Gem svar";
    gui_save_results_title => "Export report results", "Exporter les résultats du rapport", "Eksportér rapportresultater";
    gui_save_baseline_title => "Save report baseline", "Enregistrer la référence du rapport", "Gem rapport-basislinje";
    gui_unsupported_format => "Unsupported format:", "Format non pris en charge\u{a0}:", "Ikke-understøttet format:";
    gui_save => "Save", "Enregistrer", "Gem";
    gui_nothing_to_save => "Nothing to save.", "Rien à enregistrer.", "Intet at gemme.";
    gui_rename => "Rename", "Renommer", "Omdøb";
    gui_name => "Name", "Nom", "Navn";
    gui_base_url_title => "Base URL", "URL de base", "Basis-URL";
    gui_new_env_name_title => "New environment name", "Nom du nouvel environnement", "Navn på nyt miljø";
    gui_new_collection_name_title => "New collection name", "Nom de la nouvelle collection", "Navn på ny samling";
    gui_ok => "OK", "OK", "OK";
    gui_theme_editor_title => "Theme editor", "Éditeur de thème", "Tema-editor";
    gui_apply => "Apply", "Appliquer", "Anvend";
    // Reports panel.
    gui_close_reports => "Close reports", "Fermer les rapports", "Luk rapporter";
    gui_no_reports => "No reports in this session.", "Aucun rapport dans cette session.", "Ingen rapporter i denne session.";
    gui_new_report => "New report", "Nouveau rapport", "Ny rapport";
    gui_report_path => "Path:", "Chemin\u{a0}:", "Sti:";
    // PaperTrail block editor (report_editor.rs).
    gui_report_view_blocks => "Blocks", "Blocs", "Blokke";
    gui_report_view_source => "Source", "Source", "Kilde";
    gui_report_add_block => "Add block", "Ajouter un bloc", "Tilføj blok";
    gui_report_palette_blocks => "Blocks", "Blocs", "Blokke";
    gui_report_palette_hint => "Drag a block into the report", "Glissez un bloc dans le rapport", "Træk en blok ind i rapporten";
    gui_report_palette_mods => "Modifiers", "Modificateurs", "Modifikatorer";
    gui_report_palette_mods_hint => "Drop onto a block to attach", "Déposez sur un bloc pour l'attacher", "Slip på en blok for at tilføje";
    gui_report_move_up => "Move up", "Monter", "Flyt op";
    gui_report_move_down => "Move down", "Descendre", "Flyt ned";
    gui_report_delete_block => "Delete", "Supprimer", "Slet";
    gui_report_trash => "Trash (drop to delete)", "Corbeille (déposer pour supprimer)", "Papirkurv (slip for at slette)";
    node_request_title => "Configure request", "Configurer la requête", "Konfigurer forespørgsel";
    node_form_name => "Name", "Nom", "Navn";
    node_form_report => "Report", "Rapporter", "Rapportér";
    node_form_report_hint => "Include this request as report columns", "Inclure cette requête comme colonnes de rapport", "Medtag denne forespørgsel som rapportkolonner";
    node_form_response => "Response", "Réponse", "Svar";
    node_form_response_default => "Default", "Défaut", "Standard";
    node_form_alias => "Column name (AS)", "Nom de colonne (AS)", "Kolonnenavn (AS)";
    node_form_show => "Show fields (all ticked = show all)", "Afficher les champs (tous cochés = tout afficher)", "Vis felter (alle markeret = vis alle)";
    node_envs_environments => "Environments", "Environnements", "Miljøer";
    node_envs_baseline_show => "SHOW — baseline fields to carry onto every comparison row", "SHOW — champs de référence à reporter sur chaque ligne de comparaison", "SHOW — referencefelter der føres over på hver sammenligningsrække";
    node_envs_baseline_show_hint => "Ticked fields appear beside each comparison as baseline.<field>. Tick nothing for no SHOW clause.", "Les champs cochés apparaissent à côté de chaque comparaison sous la forme baseline.<champ>. Ne rien cocher = aucune clause SHOW.", "Afkrydsede felter vises ved siden af hver sammenligning som baseline.<felt>. Kryds intet af for ingen SHOW-klausul.";
    node_envs_baseline_show_applies => "applies to this baseline only", "s'applique uniquement à cette référence", "gælder kun for denne reference";
    node_envs_add => "Add environment", "Ajouter un environnement", "Tilføj miljø";
    // VARIABLE / LIST / FOLDERS / raw-line node wizards (GUI).
    node_assign_title => "Set variable", "Définir une variable", "Sæt variabel";
    node_form_var => "Variable", "Variable", "Variabel";
    node_form_value => "Value", "Valeur", "Værdi";
    node_list_title => "Configure list", "Configurer la liste", "Konfigurer liste";
    node_form_list_name => "List name", "Nom de la liste", "Listenavn";
    node_form_list_values => "Values (one per line)", "Valeurs (une par ligne)", "Værdier (én pr. linje)";
    node_folders_title => "Configure FOLDERS loop", "Configurer la boucle FOLDERS", "Konfigurer FOLDERS-løkke";
    node_raw_title => "Edit line", "Modifier la ligne", "Redigér linje";
    node_form_raw => "Statement text", "Texte de l'instruction", "Sætningstekst";
    chip_help_begin => "BEGIN — where the report starts. Blocks above it set things up; blocks below it run in order.", "BEGIN — début du rapport. Les blocs au-dessus préparent le terrain ; ceux en dessous s'exécutent dans l'ordre.", "BEGIN — hvor rapporten starter. Blokke over den gør klar; blokke under den køres i rækkefølge.";
    chip_help_hdr_collection => "COLLECTION — the requests this report runs against (its `# collection:` line). Pick one of the open collections; nothing can run until it is set.", "COLLECTION — la collection de requêtes utilisée par ce rapport (ligne `# collection:`). Choisissez une collection ouverte ; rien ne peut s'exécuter sans elle.", "COLLECTION — de forespørgsler rapporten køres mod (linjen `# collection:`). Vælg en åben samling; intet kan køre, før den er sat.";
    chip_help_hdr_output => "OUTPUT — the format the results grid is written in when the report runs: csv, json, html or xlsx. The file itself is named after the report; only the command-line runner's -o flag takes a path.", "OUTPUT — le format dans lequel la grille de résultats est écrite à l'exécution : csv, json, html ou xlsx. Le fichier porte le nom du rapport ; seule l'option -o en ligne de commande accepte un chemin.", "OUTPUT — formatet resultatgitteret skrives i, når rapporten kører: csv, json, html eller xlsx. Selve filen opkaldes efter rapporten; kun kommandolinjens -o-flag tager en sti.";
    chip_help_hdr_environment => "ENVIRONMENT — one loaded Global Environment to run with, instead of whichever is active. For comparing several, use a FOR … IN ENVS loop instead.", "ENVIRONMENT — un environnement global chargé à utiliser, au lieu de celui qui est actif. Pour en comparer plusieurs, utilisez plutôt une boucle FOR … IN ENVS.", "ENVIRONMENT — ét indlæst globalt miljø at køre med i stedet for det aktive. Skal flere sammenlignes, så brug en FOR … IN ENVS-løkke.";
    chip_help_hdr_root => "ROOT — the folder that relative file paths in this report are resolved against. Defaults to the report's own folder.", "ROOT — le dossier auquel les chemins relatifs de ce rapport se rapportent. Par défaut, le dossier du rapport.", "ROOT — mappen som relative filstier i denne rapport slås op i forhold til. Som standard rapportens egen mappe.";
    chip_help_hdr_baseline => "BASELINE — a saved run (`.baseline`) to diff this run against, filling the Result column. Ignored when the flow already compares environments.", "BASELINE — une exécution enregistrée (`.baseline`) à comparer à celle-ci, pour remplir la colonne Résultat. Ignoré si le flux compare déjà des environnements.", "BASELINE — en gemt kørsel (`.baseline`) at sammenligne denne med, som udfylder Resultat-kolonnen. Ignoreres, hvis flowet allerede sammenligner miljøer.";
    chip_help_hdr_labels => "One class of the answers this report scores: a name and the spellings that mean it, as 'Pass = pass, ok, real'.", "Une classe des réponses évaluées par ce rapport : un nom et les orthographes qui le signifient, comme 'Réussi = pass, ok, real'.", "Én klasse af de svar, denne rapport vurderer: et navn og de stavemåder, der betyder det, som 'Bestået = pass, ok, real'.";
    report_add_label_class => "Add label class", "Ajouter une classe d'étiquettes", "Tilføj etiketklasse";
    chip_help_hdr_columns => "COLUMNS — the column order of the results grid, as a comma-separated list of names. Unnamed columns follow in the order they are reported.", "COLUMNS — l'ordre des colonnes de la grille de résultats, sous forme de liste de noms séparés par des virgules. Les colonnes non citées suivent dans leur ordre d'apparition.", "COLUMNS — kolonnerækkefølgen i resultatgitteret, som en kommasepareret liste af navne. Unævnte kolonner følger i den rækkefølge, de rapporteres.";
    report_settings_help => "Report settings — these apply to the whole report rather than running as a step, so they sit above BEGIN and can't be reordered or dragged.", "Réglages du rapport — ils s'appliquent à l'ensemble du rapport au lieu de s'exécuter comme une étape ; ils se placent donc au-dessus de BEGIN et ne peuvent être ni réordonnés ni déplacés.", "Rapportindstillinger — de gælder hele rapporten i stedet for at køre som et trin, så de ligger over BEGIN og kan ikke omarrangeres eller trækkes.";
    gui_report_empty_flow => "No steps yet — drag a block here from the palette.", "Aucune étape pour l'instant — glissez un bloc ici depuis la palette.", "Ingen trin endnu — træk en blok herind fra paletten.";
    report_add_setting => "Add a report setting", "Ajouter un réglage de rapport", "Tilføj en rapportindstilling";
    report_setting_unset => "not set", "non défini", "ikke sat";
    gui_report_ws_collections => "In this workspace", "Dans cet espace de travail", "I dette arbejdsområde";
    gui_report_other_collections => "Open elsewhere", "Ouvertes ailleurs", "Åbne andre steder";
    gui_report_show_all_collections => "Show collections outside this workspace", "Afficher les collections hors de cet espace de travail", "Vis samlinger uden for dette arbejdsområde";
    gui_report_collection_unsaved => "unsaved", "non enregistrée", "ikke gemt";
    gui_report_no_collections => "No collections here yet — open one, or Browse\u{2026}", "Aucune collection ici pour l'instant — ouvrez-en une, ou Parcourir\u{2026}", "Ingen samlinger her endnu — åbn en, eller Gennemse\u{2026}";
    gui_report_browse => "Browse…", "Parcourir…", "Gennemse…";
    chip_help_flow_end => "END — where the report finishes. Everything between BEGIN and here runs, top to bottom.", "END — fin du rapport. Tout ce qui se trouve entre BEGIN et ici s'exécute, de haut en bas.", "END — hvor rapporten slutter. Alt mellem BEGIN og her køres oppefra og ned.";
    chip_help_comment => "A comment — ignored when the report runs. Kept exactly as written.", "Un commentaire — ignoré à l'exécution du rapport. Conservé tel quel.", "En kommentar — ignoreres når rapporten kører. Bevares præcis som skrevet.";
    chip_help_end => "END — closes the FOR loop above it. Drop blocks between the loop and its END to repeat them.", "END — ferme la boucle FOR au-dessus. Déposez des blocs entre la boucle et son END pour les répéter.", "END — afslutter FOR-løkken ovenfor. Slip blokke mellem løkken og dens END for at gentage dem.";
    chip_help_request => "REQUEST — runs this request from the bound collection. Pick which one from the dropdown.", "REQUEST — exécute cette requête de la collection liée. Choisissez laquelle dans la liste déroulante.", "REQUEST — kører denne forespørgsel fra den tilknyttede samling. Vælg hvilken i rullelisten.";
    chip_help_report => "REPORT — turns the line into report output. Detach it with × to run the request without reporting on it.", "REPORT — transforme la ligne en sortie de rapport. Détachez-le avec × pour exécuter la requête sans la rapporter.", "REPORT — gør linjen til rapportoutput. Fjern det med × for at køre forespørgslen uden at rapportere den.";
    chip_help_response => "RESPONSE — includes the response body as a column. RAW keeps it verbatim, PRETTY re-formats JSON. Click to change.", "RESPONSE — inclut le corps de la réponse comme colonne. RAW le garde tel quel, PRETTY reformate le JSON. Cliquez pour changer.", "RESPONSE — medtager svarets indhold som en kolonne. RAW bevarer det ordret, PRETTY omformaterer JSON. Klik for at ændre.";
    chip_help_show => "SHOW(…) — limits the report to just these fields. Without it every field is reported. Click to choose them.", "SHOW(…) — limite le rapport à ces seuls champs. Sans lui, tous les champs sont rapportés. Cliquez pour les choisir.", "SHOW(…) — begrænser rapporten til netop disse felter. Uden den rapporteres alle felter. Klik for at vælge dem.";
    chip_help_hide => "HIDE(…) — removes these fields from the report, even ones SHOW selected. Click to choose them.", "HIDE(…) — retire ces champs du rapport, même ceux sélectionnés par SHOW. Cliquez pour les choisir.", "HIDE(…) — fjerner disse felter fra rapporten, også dem SHOW valgte. Klik for at vælge dem.";
    chip_help_with => "WITH — the block below adds extra report columns from response queries. Use + to add one.", "WITH — le bloc ci-dessous ajoute des colonnes de rapport issues de requêtes sur la réponse. Utilisez + pour en ajouter une.", "WITH — blokken nedenfor tilføjer ekstra rapportkolonner fra svarforespørgsler. Brug + for at tilføje en.";
    chip_help_alias => "AS — the column heading this appears under in the report. Type a new name to rename it.", "AS — l'en-tête de colonne sous lequel ceci apparaît dans le rapport. Saisissez un nouveau nom pour le renommer.", "AS — kolonneoverskriften dette vises under i rapporten. Skriv et nyt navn for at omdøbe den.";
    chip_help_alias_required => "AS — the column heading for this computed value. It is required, so it can't be detached.", "AS — l'en-tête de colonne de cette valeur calculée. Il est obligatoire et ne peut pas être détaché.", "AS — kolonneoverskriften for denne beregnede værdi. Den er påkrævet og kan ikke fjernes.";
    chip_help_var => "The value reported into the column — a variable set earlier in the flow, or a loop's variable.", "La valeur rapportée dans la colonne — une variable définie plus haut dans le flux, ou la variable d'une boucle.", "Værdien der rapporteres i kolonnen — en variabel sat tidligere i flowet eller en løkkes variabel.";
    chip_help_computed => "A computed column: the quoted template is expanded with {{variables}} and reported under its AS name.", "Une colonne calculée : le modèle entre guillemets est développé avec {{variables}} et rapporté sous son nom AS.", "En beregnet kolonne: den citerede skabelon udvides med {{variabler}} og rapporteres under sit AS-navn.";
    chip_help_assign => "Sets a variable for the rest of the flow. Open it to edit the name and value.", "Définit une variable pour la suite du flux. Ouvrez-le pour modifier le nom et la valeur.", "Sætter en variabel for resten af flowet. Åbn den for at redigere navn og værdi.";
    chip_help_list => "Declares a named list of values that a FOR loop can iterate over. Open it to edit the entries.", "Déclare une liste nommée de valeurs qu'une boucle FOR peut parcourir. Ouvrez-la pour modifier les entrées.", "Erklærer en navngiven liste af værdier, som en FOR-løkke kan gennemløbe. Åbn den for at redigere posterne.";
    chip_help_for => "FOR — repeats everything up to its END once per item. Open it to change the loop variable and its source.", "FOR — répète tout jusqu'à son END une fois par élément. Ouvrez-le pour changer la variable de boucle et sa source.", "FOR — gentager alt frem til sit END én gang pr. element. Åbn den for at ændre løkkevariablen og dens kilde.";
    chip_help_loop_var => "The loop variable — the name each item is bound to inside the loop.", "La variable de boucle — le nom auquel chaque élément est lié dans la boucle.", "Løkkevariablen — navnet hvert element bindes til inde i løkken.";
    chip_help_loop_dir => "The folder this loop reads from.", "Le dossier dans lequel cette boucle lit.", "Mappen denne løkke læser fra.";
    chip_help_loop_glob => "A filename pattern, like *.json or report-*.csv. The loop skips any file whose name doesn't match it. Leave it blank to use every file in the folder.", "Un motif de nom de fichier, comme *.json ou rapport-*.csv. La boucle ignore tout fichier dont le nom n'y correspond pas. Laissez vide pour utiliser tous les fichiers du dossier.", "Et filnavnsmønster, som *.json eller rapport-*.csv. Løkken springer alle filer over, hvis navn ikke matcher. Lad det stå tomt for at bruge alle filer i mappen.";
    // Short placeholders shown *inside* the empty boxes. Kept to a word or a
    // literal example: the box is a few characters wide, so the full sentence
    // (the `chip_help_*` strings above, now shown on hover) only ever appeared
    // as an unreadable stub like "Only files ...".
    gui_report_loop_var_hint => "name", "nom", "navn";
    gui_report_loop_dir_hint => "folder", "dossier", "mappe";
    gui_report_loop_glob_hint => "*.json", "*.json", "*.json";
    // The folder/file picker button beside a loop's path box.
    chip_help_loop_pick_folder => "Browse for the folder to loop over", "Parcourir pour choisir le dossier à parcourir", "Gennemse efter mappen, der skal løbes igennem";
    chip_help_loop_pick_file => "Browse for the file to read the rows from", "Parcourir pour choisir le fichier dont les lignes seront lues", "Gennemse efter filen, som rækkerne skal læses fra";
    gui_pick_loop_folder => "Choose the folder to loop over", "Choisissez le dossier à parcourir", "Vælg mappen der skal løkkes over";
    gui_pick_loop_file => "Choose the file to loop over", "Choisissez le fichier à parcourir", "Vælg filen der skal løkkes over";
    chip_help_parallel => "PARALLEL — runs the loop's iterations at the same time. Type a number to cap how many run at once; leave it blank to use the default.", "PARALLEL — exécute les itérations de la boucle en même temps. Saisissez un nombre pour limiter combien s'exécutent à la fois ; laissez vide pour la valeur par défaut.", "PARALLEL — kører løkkens gennemløb samtidigt. Skriv et tal for at begrænse hvor mange der kører ad gangen; lad det stå tomt for standarden.";
    chip_help_for_envs => "FOR … IN ENVS — runs everything up to its END once per environment listed, with that environment's variables active. The name after FOR is what each run is labelled with in the report, not a value you set. Open it to choose the environments.", "FOR … IN ENVS — exécute tout jusqu'à son END une fois par environnement listé, avec les variables de cet environnement actives. Le nom après FOR sert d'étiquette à chaque exécution dans le rapport, ce n'est pas une valeur que vous définissez. Ouvrez-le pour choisir les environnements.", "FOR … IN ENVS — kører alt frem til sit END én gang pr. angivet miljø, med det miljøs variabler aktive. Navnet efter FOR er det, hver kørsel navngives med i rapporten, ikke en værdi du sætter. Åbn den for at vælge miljøerne.";
    chip_help_baseline_show => "SHOW — the BASELINE's fields carried onto every comparison row. Belongs to the BASELINE beside it; edit it in the ENVS form.", "SHOW — les champs de la BASELINE reportés sur chaque ligne de comparaison. Appartient à la BASELINE voisine ; modifiable dans le formulaire ENVS.", "SHOW — BASELINE-felterne der føres over på hver sammenligningsrække. Hører til den BASELINE ved siden af; redigér den i ENVS-formularen.";
    chip_help_baseline => "BASELINE — the environment the others are compared against. Pick it from the dropdown.", "BASELINE — l'environnement auquel les autres sont comparés. Choisissez-le dans la liste déroulante.", "BASELINE — det miljø de andre sammenlignes med. Vælg det i rullelisten.";
    chip_help_comparison => "COMPARISON — an environment compared against the baseline. Pick it from the dropdown.", "COMPARISON — un environnement comparé à la référence. Choisissez-le dans la liste déroulante.", "COMPARISON — et miljø der sammenlignes med referencen. Vælg det i rullelisten.";
    chip_help_image => "IMAGE — this column holds a picture, embedded in the report at this size rather than written as a file path. Open the block to change the size.", "IMAGE — cette colonne contient une image, intégrée au rapport à cette taille plutôt qu'écrite comme chemin de fichier. Ouvrez le bloc pour changer la taille.", "IMAGE — denne kolonne indeholder et billede, indlejret i rapporten i denne størrelse i stedet for skrevet som en filsti. Åbn blokken for at ændre størrelsen.";
    chip_help_truth => "TRUTH — the value this column is expected to hold, so the run can be scored right or wrong against it.", "TRUTH — la valeur attendue de cette colonne, afin que l'exécution puisse être jugée correcte ou non.", "TRUTH — den værdi, denne kolonne forventes at have, så kørslen kan bedømmes rigtig eller forkert.";
    chip_help_detail => "DETAIL — keep this column out of the grid and show it in a row's drill-down instead.", "DETAIL — garder cette colonne hors du tableau et l'afficher dans le détail d'une ligne.", "DETAIL — hold denne kolonne uden for tabellen og vis den i rækkens detaljevisning i stedet.";
    chip_help_statistics => "STATISTICS — summary rows (count, mean, …) for this column. Open the block to choose which.", "STATISTICS — lignes de synthèse (comptage, moyenne, …) pour cette colonne. Ouvrez le bloc pour les choisir.", "STATISTICS — opsummeringsrækker (antal, gennemsnit, …) for denne kolonne. Åbn blokken for at vælge hvilke.";
    chip_help_drag_gesture => "Drag this chip out (or click ×) to remove just it · drop it on another line to move it there (Shift to copy) · Ctrl+drag to move the whole line", "Faites glisser cette puce (ou cliquez sur ×) pour ne retirer qu'elle · déposez-la sur une autre ligne pour l'y déplacer (Maj pour copier) · Ctrl+glisser pour déplacer toute la ligne", "Træk denne chip ud (eller klik på ×) for kun at fjerne den · slip den på en anden linje for at flytte den dertil (Shift for at kopiere) · Ctrl+træk for at flytte hele linjen";
    chip_help_roles_fixed => "Environments for the comparison. This form (several environments, or a saved FILE snapshot) is edited in the loop's form — open the block to change it.", "Environnements de la comparaison. Cette forme (plusieurs environnements, ou un instantané FILE enregistré) se modifie dans le formulaire de la boucle — ouvrez le bloc pour la changer.", "Miljøer til sammenligningen. Denne form (flere miljøer eller et gemt FILE-øjebliksbillede) redigeres i løkkens formular — åbn blokken for at ændre den.";
    node_form_hide => "Hide fields", "Masquer des champs", "Skjul felter";
    node_form_hide_hint => "Ticked fields are removed from the report, even if shown above", "Les champs cochés sont retirés du rapport, même s'ils sont affichés ci-dessus", "Markerede felter fjernes fra rapporten, også selvom de vises ovenfor";
    node_form_statistics => "Statistics", "Statistiques", "Statistik";
    node_form_statistics_hint => "Summary rows added under this column", "Lignes de synthèse ajoutées sous cette colonne", "Opsummeringsrækker tilføjet under denne kolonne";
    node_form_parallel_degree => "max", "max", "maks";
    node_form_parallel_degree_label => "Max parallel steps (blank = use the default)", "Étapes parallèles max. (vide = valeur par défaut)", "Maks. parallelle trin (tomt = brug standarden)";
    // WITH-field wizard + inline block-editor helpers (GUI).
    node_with_title => "Report field (WITH)", "Champ de rapport (WITH)", "Rapportfelt (WITH)";
    node_with_name => "Column name", "Nom de colonne", "Kolonnenavn";
    node_with_query => "Query", "Requête", "Forespørgsel";
    node_with_query_hint => "e.g. HttpStatus or jsonpath \"$.field\"", "ex. HttpStatus ou jsonpath \"$.field\"", "f.eks. HttpStatus eller jsonpath \"$.field\"";
    gui_report_with_add => "Add field", "Ajouter un champ", "Tilføj felt";
    gui_report_filter_hint => "Filter…", "Filtrer…", "Filtrér…";
    gui_report_alias_hint => "alias", "alias", "alias";
    gui_report_view_results => "Results", "Résultats", "Resultater";
    gui_report_run => "Run", "Exécuter", "Kør";
    gui_report_stop => "Stop", "Arrêter", "Stop";
    gui_report_export => "Export…", "Exporter…", "Eksportér…";
    gui_report_open_export => "Open", "Ouvrir", "Åbn";
    gui_report_export_go => "Export", "Exporter", "Eksportér";
    gui_report_export_format => "Format:", "Format :", "Format:";
    gui_report_save_baseline => "Baseline…", "Référence…", "Basislinje…";
    gui_report_no_results => "No results yet — press Run to execute this report.", "Aucun résultat pour l'instant — appuyez sur Exécuter pour lancer ce rapport.", "Ingen resultater endnu — tryk på Kør for at køre denne rapport.";
    gui_report_running => "Running…", "Exécution…", "Kører…";
    // `{c}` is substituted with the expander caret *glyph* at draw time: the
    // GUI draws its icons from the Phosphor icon font, which a bare Unicode
    // triangle written here is not in -- it came out as a tofu box.
    gui_report_cell_hint => "Click a row marked {c} to open its details, or any other cell to inspect its full value", "Cliquez sur une ligne marquée {c} pour ouvrir ses détails, ou sur toute autre cellule pour voir sa valeur complète", "Klik på en række markeret med {c} for at åbne dens detaljer, eller på en anden celle for at se dens fulde værdi";
    gui_report_cell_copy_full => "Copy full value", "Copier la valeur complète", "Kopiér fuld værdi";
    // The results view's filter bar and metric cards. `RowFilter::label` is
    // deliberately English -- it names buttons in an exported document that has
    // no language -- so the in-app bar labels its own buttons from here.
    report_filter_all => "All", "Tout", "Alle";
    report_filter_differences => "Differences", "Différences", "Forskelle";
    report_filter_incorrect => "Incorrect", "Incorrect", "Forkerte";
    report_filter_regressions => "Regressions", "Régressions", "Regressioner";
    report_find_placeholder => "Find in rows…", "Rechercher dans les lignes…", "Søg i rækker…";
    report_rows_shown => "{shown} of {total} rows", "{shown} lignes sur {total}", "{shown} af {total} rækker";
    // The terminal results view's pinned summary: one metric line per scored
    // column, and the filter line `f` cycles. The GUI says the same things in
    // cards and buttons; a terminal has one line to say them in.
    // Sits in the results panel's title bar (not in the grid): it describes the
    // pane, and the key that changes it is named in the title's hint and in the
    // help overlay rather than repeated here on every screen.
    report_filter_title => "Filter: {f}, {r}", "Filtre\u{a0}: {f}, {r}", "Filter: {f}, {r}";
    report_metric_compared => "Compared", "Comparés", "Sammenlignet";
    report_metric_incorrect => "Incorrect", "Incorrects", "Forkerte";
    report_metric_accuracy => "Accuracy", "Exactitude", "Nøjagtighed";
    // How a run moved against its baseline. Accuracy alone can't tell two 98%
    // runs apart when one of them fixed three rows and broke three others.
    report_metric_movement => "Movement", "Évolution", "Bevægelse";
    report_metric_fixed => "Fixed", "Corrigés", "Rettede";
    report_metric_regressed => "Regressed", "Régressions", "Forværrede";
    report_metric_still_wrong => "Still wrong", "Toujours faux", "Stadig forkerte";
    report_metric_nothing_moved => "Nothing moved", "Rien n'a changé", "Intet flyttede sig";
    report_detail_title => "Row {n} details", "Détails de la ligne {n}", "Detaljer for række {n}";
    report_detail_close => "Close this row's details", "Fermer les détails de cette ligne", "Luk denne rækkes detaljer";
    report_detail_changed => "{c} — changed fields", "{c} — champs modifiés", "{c} — ændrede felter";
    report_matrix_caption => "Rows: ground truth. Columns: reported value. {n} scored row(s).", "Lignes\u{a0}: vérité terrain. Colonnes\u{a0}: valeur rapportée. {n} ligne(s) évaluée(s).", "Rækker: sandhed. Kolonner: rapporteret værdi. {n} bedømt(e) række(r).";
    report_matrix_all_matched => "Every scored row matched its ground truth.", "Chaque ligne évaluée correspond à sa vérité terrain.", "Alle bedømte rækker matchede deres sandhed.";
    help_report_matrix_cell => "Show the rows this count is made of", "Afficher les lignes qui composent ce total", "Vis de rækker, dette tal består af";
    help_report_filter => "Show only the rows this filter selects", "N'afficher que les lignes sélectionnées par ce filtre", "Vis kun de rækker, dette filter vælger";
    // Git remote flow (used by remote.rs).
    gui_git_repo_url => "Repository URL", "URL du dépôt", "Lager-URL";
    gui_git_token => "Access token (optional)", "Jeton d'accès (facultatif)", "Adgangstoken (valgfrit)";
    gui_git_connect => "Connect", "Se connecter", "Forbind";
    gui_git_branches => "Branches", "Branches", "Grene";
    gui_git_tags => "Tags", "Étiquettes", "Tags";
    gui_git_browse_files => "Browse files", "Parcourir les fichiers", "Gennemse filer";
    gui_git_back => "Back", "Retour", "Tilbage";
    gui_git_next => "Next", "Suivant", "Næste";
    gui_git_tag => "Tag", "Étiquette", "Tag";
    gui_git_existing_branch => "Use an existing branch…", "Utiliser une branche existante…", "Brug en eksisterende gren…";
    gui_git_saved => "Saved to Git.", "Enregistré sur Git.", "Gemt til Git.";
    // -- Postman bulk import ------------------------------------------------
    postman_busy_listing => "Listing your Postman workspaces…", "Liste des espaces de travail Postman…", "Henter dine Postman-workspaces…";
    postman_busy_planning => "Checking what that workspace holds…", "Vérification du contenu de cet espace de travail…", "Undersøger hvad dette workspace indeholder…";
    postman_busy_downloading => "Downloading from Postman…", "Téléchargement depuis Postman…", "Henter fra Postman…";
    postman_err_key_required => "A Postman API key is needed.", "Une clé d'API Postman est nécessaire.", "Der kræves en Postman-API-nøgle.";
    postman_err_key_ref => "Could not resolve that reference. Check it, and that op / aws is installed and signed in.", "Impossible de résoudre cette référence. Vérifiez-la, ainsi que l’installation et la connexion de op / aws.", "Kunne ikke slå referencen op. Tjek den, og at op / aws er installeret og logget ind.";
    postman_err_bad_workspace => "That is not a workspace id or a Postman workspace address.", "Ce n'est ni un identifiant d'espace de travail ni une adresse d'espace de travail Postman.", "Det er hverken et workspace-id eller en Postman-workspace-adresse.";
    postman_err_dest_required => "Choose a folder to import into.", "Choisissez un dossier de destination.", "Vælg en mappe at importere til.";
    postman_err_nothing_selected => "Choose collections, environments, or both.", "Choisissez les collections, les environnements, ou les deux.", "Vælg samlinger, miljøer eller begge dele.";
    postman_err_no_workspace => "No workspace was chosen.", "Aucun espace de travail n'a été choisi.", "Der blev ikke valgt noget workspace.";
    postman_err_no_workspaces => "This key can see no workspaces. A key carries its owner’s access, so check your Postman account is a member.", "Cette clé ne voit aucun espace de travail. Une clé porte les accès de son propriétaire : vérifiez que votre compte Postman en est membre.", "Denne nøgle kan ikke se nogen workspaces. En nøgle bærer ejerens adgang, så tjek at din Postman-konto er medlem.";
    postman_err_worker_ended => "The import stopped unexpectedly.", "L'importation s'est arrêtée de façon inattendue.", "Importen stoppede uventet.";
    postman_word_collection => "collection", "collection", "samling";
    postman_word_collections => "collections", "collections", "samlinger";
    postman_word_environment => "environment", "environnement", "miljø";
    postman_word_environments => "environments", "environnements", "miljøer";
    postman_unit_seconds => "seconds", "secondes", "sekunder";
    postman_unit_minutes => "minutes", "minutes", "minutter";
    postman_title => "Import from Postman", "Importer depuis Postman", "Importér fra Postman";
    postman_connect_hint => "Enter connect · Esc cancel", "Entrée connecter · Échap annuler", "Enter forbind · Esc annuller";
    postman_key_label => "API key", "Clé d’API", "API-nøgle";
    postman_key_source_label => "Key from", "Clé depuis", "Nøgle fra";
    postman_key_source_paste => "Typed here", "Saisie directe", "Indtastet her";
    postman_key_source_op => "1Password", "1Password", "1Password";
    postman_key_source_ssm => "AWS Parameter Store", "AWS Parameter Store", "AWS Parameter Store";
    postman_key_source_env => "Environment variable", "Variable d'environnement", "Miljøvariabel";
    postman_key_label_op => "1Password item", "Élément 1Password", "1Password-element";
    postman_key_label_ssm => "Parameter", "Paramètre", "Parameter";
    postman_key_label_env => "Variable", "Variable", "Variabel";
    postman_key_hint_op => "Private/Postman/credential", "Private/Postman/credential", "Private/Postman/credential";
    postman_key_hint_ssm => "/prod/postman/api-key", "/prod/postman/api-key", "/prod/postman/api-key";
    postman_key_hint_env => "POSTMAN_API_KEY", "POSTMAN_API_KEY", "POSTMAN_API_KEY";
    postman_key_hint => "PMAK-… from go.postman.co/settings/me/api-keys", "PMAK-… depuis go.postman.co/settings/me/api-keys", "PMAK-… fra go.postman.co/settings/me/api-keys";
    postman_workspace_label => "Workspace", "Espace de travail", "Workspace";
    postman_workspace_hint => "blank: choose from a list", "vide : choisir dans une liste", "tom: vælg fra en liste";
    postman_base_url_label => "API host", "Hôte de l’API", "API-vært";
    postman_base_url_hint => "api.getpostman.com · EU: api.eu.postman.com", "api.getpostman.com · UE : api.eu.postman.com", "api.getpostman.com · EU: api.eu.postman.com";
    postman_pick_workspace => "Choose a workspace", "Choisissez un espace de travail", "Vælg et workspace";
    postman_include_collections => "Collections", "Collections", "Samlinger";
    postman_include_environments => "Environments", "Environnements", "Miljøer";
    postman_dest_label => "Import into", "Importer dans", "Importér til";
    postman_dest_unset => "(no folder chosen)", "(aucun dossier choisi)", "(ingen mappe valgt)";
    postman_browse => "[Enter to choose…]", "[Entrée pour choisir…]", "[Enter for at vælge…]";
    postman_dest_folder => "Import into", "Importer dans", "Importér til";
    postman_options_hint_dest => "Enter choose folder · Esc back", "Entrée choisir le dossier · Échap retour", "Enter vælg mappe · Esc tilbage";
    postman_options_hint_toggle => "Space change · Esc back", "Espace changer · Échap retour", "Mellemrum skift · Esc tilbage";
    postman_options_hint_import => "Enter import · Esc back", "Entrée importer · Échap retour", "Enter importér · Esc tilbage";
    postman_format_label => "Format", "Format", "Format";
    postman_format_raw => "Postman JSON (keeps everything)", "JSON Postman (conserve tout)", "Postman-JSON (bevarer alt)";
    postman_format_hurl => "Convert to Hurl (.hurl and .vars)", "Convertir en Hurl (.hurl et .vars)", "Konvertér til Hurl (.hurl og .vars)";
    postman_format_hurl_note => "What Hurl cannot express is listed in CONVERSION-NOTES.md.", "Ce que Hurl ne peut exprimer est listé dans CONVERSION-NOTES.md.", "Hvad Hurl ikke kan udtrykke, listes i CONVERSION-NOTES.md.";
    postman_overwrite => "Replace the folder if it already exists", "Remplacer le dossier s'il existe déjà", "Erstat mappen hvis den allerede findes";
    postman_confirm_title => "Ready to import", "Prêt à importer", "Klar til at importere";
    postman_error_hint => "Esc back", "Échap retour", "Esc tilbage";
    postman_confirm_hint => "Enter import · Esc back", "Entrée importer · Échap retour", "Enter importér · Esc tilbage";
    postman_rate_limit_note => "Paced to stay inside Postman’s rate limit.", "Rythme adapté à la limite de débit de Postman.", "Tempo tilpasset Postmans hastighedsgrænse.";
    postman_estimate => "About", "Environ", "Cirka";
    postman_budget_warning => "This would use a large share of this account's remaining monthly API budget.", "Cela consommerait une grande partie du budget d'API mensuel restant de ce compte.", "Dette ville bruge en stor del af kontoens resterende månedlige API-budget.";
    postman_waiting_paced => "Pausing to stay within Postman's rate limit", "Pause pour respecter la limite de débit de Postman", "Pauser for at holde sig inden for Postmans hastighedsgrænse";
    postman_waiting_limited => "Postman asked us to wait", "Postman nous a demandé d'attendre", "Postman bad os vente";
    postman_remaining => "left", "restant", "tilbage";
    postman_done_title => "Import complete", "Importation terminée", "Import fuldført";
    postman_skipped => "could not be fetched and were skipped", "n'ont pas pu être récupérés et ont été ignorés", "kunne ikke hentes og blev sprunget over";
    postman_notes_written => "Some things could not be converted to Hurl — see CONVERSION-NOTES.md in the imported folder.", "Certains éléments n'ont pas pu être convertis en Hurl — voir CONVERSION-NOTES.md dans le dossier importé.", "Nogle ting kunne ikke konverteres til Hurl — se CONVERSION-NOTES.md i den importerede mappe.";
    postman_start => "Import", "Importer", "Importér";
    gui_git_filter => "Filter", "Filtrer", "Filter";
    gui_git_load => "Load", "Charger", "Indlæs";
    gui_git_save => "Save", "Enregistrer", "Gem";
    gui_git_branch => "Branch", "Branche", "Gren";
    gui_git_path => "Path in repo", "Chemin dans le dépôt", "Sti i lageret";
    gui_git_commit_message => "Commit message", "Message de commit", "Commit-besked";
    gui_git_show_all_files => "Show all files", "Afficher tous les fichiers", "Vis alle filer";
    gui_git_pick_ref => "Pick a branch or tag", "Choisir une branche ou une étiquette", "Vælg en gren eller et tag";
    gui_git_load_title => "Load from Git", "Charger depuis Git", "Indlæs fra Git";
    gui_git_save_collection_title => "Save collection to Git", "Enregistrer la collection sur Git", "Gem samling til Git";
    gui_git_save_workspace_title => "Save workspace to Git", "Enregistrer l'espace de travail sur Git", "Gem workspace til Git";
    gui_git_save_report_title => "Save report to Git", "Enregistrer le rapport sur Git", "Gem rapport til Git";
    gui_git_fetched_at => "Fetched at", "Récupéré à", "Hentet ved";
    gui_git_no_files => "No .hurl or .vars files match. Enable “Show all files” to browse everything.", "Aucun fichier .hurl ou .vars ne correspond. Activez « Afficher tous les fichiers » pour tout parcourir.", "Ingen .hurl- eller .vars-filer matcher. Slå “Vis alle filer” til for at gennemse alt.";
    gui_git_checkout_gone => "The temporary checkout was cleaned up. Go Back and Browse files again to retry.", "L'extraction temporaire a été nettoyée. Revenez en arrière et parcourez à nouveau les fichiers pour réessayer.", "Den midlertidige udtjekning blev ryddet op. Gå tilbage og gennemse filer igen for at prøve igen.";
    gui_git_err_no_file => "No file was selected.", "Aucun fichier n'a été sélectionné.", "Ingen fil blev valgt.";
    gui_git_err_no_ref => "No branch or tag was selected.", "Aucune branche ou étiquette n'a été sélectionnée.", "Ingen gren eller tag blev valgt.";
    gui_git_err_not_env => "The selected file is not a valid .vars environment.", "Le fichier sélectionné n'est pas un environnement .vars valide.", "Den valgte fil er ikke et gyldigt .vars-miljø.";
    gui_git_err_not_collection => "The selected file is not a valid collection.", "Le fichier sélectionné n'est pas une collection valide.", "Den valgte fil er ikke en gyldig samling.";
    gui_git_err_collection_missing => "Collection not found.", "Collection introuvable.", "Samling ikke fundet.";
    gui_git_err_collection_closed => "Collection was closed before the save finished.", "La collection a été fermée avant la fin de l'enregistrement.", "Samlingen blev lukket, før lagringen var færdig.";
    gui_git_err_url_required => "Repository URL is required.", "L'URL du dépôt est requise.", "Lager-URL er påkrævet.";
    gui_git_err_pick_ref_first => "Choose a branch or tag first.", "Choisissez d'abord une branche ou une étiquette.", "Vælg først en gren eller et tag.";
    gui_git_err_browse_again => "Browse files again before loading.", "Parcourez à nouveau les fichiers avant de charger.", "Gennemse filer igen før indlæsning.";
    gui_git_err_pick_file => "Choose a file to load.", "Choisissez un fichier à charger.", "Vælg en fil at indlæse.";
    gui_git_err_path_required => "Path in repository is required.", "Le chemin dans le dépôt est requis.", "Sti i lageret er påkrævet.";
    gui_git_err_path_relative => "Path must be relative and must not contain “..”.", "Le chemin doit être relatif et ne doit pas contenir « .. ».", "Stien skal være relativ og må ikke indeholde “..”.";
    gui_git_load_report_title => "Load report from Git", "Charger un rapport depuis Git", "Indlæs rapport fra Git";
    gui_git_load_workspace_title => "Load workspace from Git", "Charger un espace de travail depuis Git", "Indlæs workspace fra Git";
    gui_git_ws_pick_filter => "Choose which files to download. Nothing else in the repository is ever fetched.", "Choisissez les fichiers à télécharger. Rien d'autre dans le dépôt n'est jamais récupéré.", "Vælg hvilke filer der skal hentes. Intet andet i lageret bliver nogensinde hentet.";
    gui_git_ws_match_count => "{n} of {total} files match", "{n} fichiers sur {total} correspondent", "{n} af {total} filer matcher";
    gui_git_ws_download => "Download", "Télécharger", "Hent";
    gui_git_ws_storage_title => "Keep this workspace?", "Conserver cet espace de travail ?", "Behold dette workspace?";
    gui_git_ws_folder_name => "Folder name", "Nom du dossier", "Mappenavn";
    gui_git_err_ws_no_matches => "No files in this repository matched that filter.", "Aucun fichier de ce dépôt ne correspond à ce filtre.", "Ingen filer i dette lager matchede det filter.";
    gui_git_err_ws_name_required => "A folder name is required.", "Un nom de dossier est requis.", "Et mappenavn er påkrævet.";
    gui_git_err_ws_exists => "That folder already exists — choose another name.", "Ce dossier existe déjà — choisissez un autre nom.", "Den mappe findes allerede — vælg et andet navn.";
    gui_close_git_workspace_title => "Close downloaded workspace", "Fermer l'espace de travail téléchargé", "Luk downloadet workspace";
    gui_workspace_reload_title => "Workspace files are missing", "Les fichiers de l'espace de travail sont introuvables", "Workspace-filer mangler";
    gui_workspace_reload_yes => "Redownload", "Retélécharger", "Hent igen";
    gui_workspace_reload_no => "Leave it empty", "Le laisser vide", "Lad den være tom";

    // ── Report validation ────────────────────────────────────────────────
    // These read in the terms of whatever front-end is showing them (a
    // "collection setting", not a `# collection:` header), because the block
    // editor never shows the source syntax the directive is written in.
    // `{}` placeholders are filled in order by `fill`.
    diag_collection_unset => "No collection chosen — a report has nothing to run against until one is set.", "Aucune collection choisie — un rapport n'a rien à exécuter tant qu'aucune n'est définie.", "Ingen samling valgt — en rapport har intet at køre mod, før en er sat.";
    diag_output_unsupported => "Unsupported output format '{}' — supported formats are {}.", "Format de sortie '{}' non pris en charge — les formats acceptés sont {}.", "Outputformatet '{}' understøttes ikke — de understøttede formater er {}.";
    diag_duplicate_column => "Two columns are both headed '{}' — give each one a distinct name with AS.", "Deux colonnes portent le même titre '{}' — donnez à chacune un nom distinct avec AS.", "To kolonner har begge overskriften '{}' — giv hver af dem et særskilt navn med AS.";
    diag_labels_malformed => "The label setting '{}' declares nothing — write it as 'Name = synonym, synonym'.", "Le réglage d'étiquettes '{}' ne déclare rien — écrivez-le sous la forme 'Nom = synonyme, synonyme'.", "Etiketindstillingen '{}' erklærer ingenting — skriv den som 'Navn = synonym, synonym'.";
    diag_labels_conflict => "'{}' is claimed by both '{}' and '{}' — it counts as '{}'.", "'{}' est revendiqué à la fois par '{}' et '{}' — il compte comme '{}'.", "'{}' hævdes af både '{}' og '{}' — den tælles som '{}'.";
    diag_truth_empty => "Column '{}' has an empty ground truth, so it will never be scored.", "La colonne '{}' a une vérité terrain vide, elle ne sera donc jamais évaluée.", "Kolonnen '{}' har en tom grundsandhed, så den bliver aldrig vurderet.";
    diag_truth_on_image => "Column '{}' is shown as a picture, so its ground truth compares the address of the picture rather than what it shows.", "La colonne '{}' est affichée comme une image, sa vérité terrain compare donc l'adresse de l'image plutôt que son contenu.", "Kolonnen '{}' vises som et billede, så dens grundsandhed sammenligner billedets adresse frem for det, det viser.";
    diag_environment_unset => "The environment setting is empty — either name an environment or remove the setting.", "Le réglage d'environnement est vide — nommez un environnement ou supprimez le réglage.", "Miljøindstillingen er tom — navngiv et miljø, eller fjern indstillingen.";
    diag_environment_not_loaded => "Environment '{}' is not loaded.", "L'environnement '{}' n'est pas chargé.", "Miljøet '{}' er ikke indlæst.";
    diag_baseline_ignored => "The baseline setting is ignored, because this report already compares a BASELINE environment with COMPARISON environments.", "Le réglage baseline est ignoré, car ce rapport compare déjà un environnement BASELINE à des environnements COMPARISON.", "Baseline-indstillingen ignoreres, fordi denne rapport allerede sammenligner et BASELINE-miljø med COMPARISON-miljøer.";
    diag_baseline_missing => "The baseline snapshot '{}' was not found at {}.", "L'instantané baseline '{}' est introuvable à {}.", "Baseline-øjebliksbilledet '{}' blev ikke fundet på {}.";
    diag_no_columns => "This report emits no columns, so it will produce an empty table — attach REPORT to a request to make it one.", "Ce rapport ne produit aucune colonne, il donnera donc un tableau vide — attachez REPORT à une requête pour en créer une.", "Denne rapport udsender ingen kolonner, så den giver en tom tabel — sæt REPORT på en forespørgsel for at lave en.";
    diag_collection_not_loaded => "The collection isn't loaded, so request names can't be checked yet.", "La collection n'est pas chargée, les noms de requêtes ne peuvent donc pas encore être vérifiés.", "Samlingen er ikke indlæst, så forespørgselsnavne kan ikke tjekkes endnu.";
    diag_list_shadowed => "LIST '{}' hides an earlier list of the same name.", "LIST '{}' masque une liste antérieure du même nom.", "LIST '{}' skjuler en tidligere liste med samme navn.";
    diag_show_unknown => "SHOW field '{}' on request '{}' isn't a field that request produces, so it will be ignored.", "Le champ SHOW '{}' de la requête '{}' n'est pas un champ produit par cette requête, il sera donc ignoré.", "SHOW-feltet '{}' på forespørgslen '{}' er ikke et felt, den forespørgsel producerer, så det ignoreres.";
    diag_show_hide_conflict => "Field '{}' is in both SHOW and HIDE — these conflict.", "Le champ '{}' figure à la fois dans SHOW et HIDE — ces clauses sont contradictoires.", "Feltet '{}' er både i SHOW og HIDE — de er i konflikt.";
    diag_hide_unknown => "HIDE field '{}' on request '{}' isn't a field that request produces.", "Le champ HIDE '{}' de la requête '{}' n'est pas un champ produit par cette requête.", "HIDE-feltet '{}' på forespørgslen '{}' er ikke et felt, den forespørgsel producerer.";
    diag_request_ambiguous_title => "Request '{}' is ambiguous — {} requests share that title.", "La requête '{}' est ambiguë — {} requêtes portent ce titre.", "Forespørgslen '{}' er tvetydig — {} forespørgsler deler den titel.";
    report_add_helper_collection => "HELPER COLLECTION…", "COLLECTION D'APPOINT…", "HJÆLPESAMLING…";
    report_alias_unset => "alias", "alias", "alias";
    // The two halves of a `# labels:` class. The hints are examples rather than
    // descriptions: the whole point of splitting the directive into two fields
    // is to show what goes where, and "Low Risk" teaches that faster than
    // "canonical label" does.
    report_label_class_unset => "Low Risk", "Risque faible", "Lav risiko";
    report_label_synonyms_unset => "real, genuine, pass", "réel, authentique, succès", "ægte, autentisk, bestået";
    report_label_synonyms_help => "Other spellings that mean this label. A truth of \"real\" and a response of \"Low Risk\" score as a match when both are listed here.", "Autres formulations qui désignent cette étiquette. Une vérité « real » et une réponse « Low Risk » comptent comme identiques si les deux figurent ici.", "Andre stavemåder, der betyder denne etiket. En sandhed \"real\" og et svar \"Low Risk\" tæller som ens, når begge står her.";
    report_helper_collection_help => "Another collection whose requests this report can call, written 'path AS alias'. Its requests are then used as 'alias/request'. Handy for a request that supports the report but isn't part of the API being tested — it stays out of Run All.", "Une autre collection dont ce rapport peut appeler les requêtes, écrite 'chemin AS alias'. Ses requêtes s'utilisent alors comme 'alias/requête'. Pratique pour une requête qui sert au rapport sans faire partie de l'API testée : elle reste hors de « Tout exécuter ».", "En anden samling, hvis forespørgsler denne rapport kan kalde, skrevet 'sti AS alias'. Dens forespørgsler bruges så som 'alias/forespørgsel'. Nyttigt til en forespørgsel, der understøtter rapporten uden at være en del af den API, der testes — den holdes uden for Kør alle.";
    diag_collection_primary_alias => "The first '# collection:' is the report's own collection, so it takes no 'AS' alias. Aliases name the extra helper collections below it.", "La première '# collection:' est la collection du rapport : elle ne prend pas d'alias 'AS'. Les alias nomment les collections d'appoint qui la suivent.", "Den første '# collection:' er rapportens egen samling og tager derfor ikke et 'AS'-alias. Aliasser navngiver de ekstra hjælpesamlinger under den.";
    diag_collection_alias_missing => "Helper collection '{}' needs an alias: write '# collection: {0} AS name'. Its requests are then used as 'name/request'.", "La collection d'appoint '{}' a besoin d'un alias : écrivez '# collection: {0} AS nom'. Ses requêtes s'utilisent alors comme 'nom/requête'.", "Hjælpesamlingen '{}' mangler et alias: skriv '# collection: {0} AS navn'. Dens forespørgsler bruges så som 'navn/forespørgsel'.";
    diag_collection_alias_invalid => "'{}' is not a usable collection alias. Use letters, digits and underscores, starting with a letter or underscore.", "'{}' n'est pas un alias de collection utilisable. Utilisez des lettres, des chiffres et des tirets bas, en commençant par une lettre ou un tiret bas.", "'{}' kan ikke bruges som samlingsalias. Brug bogstaver, tal og understregninger, og begynd med et bogstav eller en understregning.";
    diag_collection_alias_duplicate => "Two helper collections are both called '{}'. Give each one its own alias so 'alias/request' names exactly one request.", "Deux collections d'appoint s'appellent '{}'. Donnez à chacune son propre alias pour que 'alias/requête' désigne une seule requête.", "To hjælpesamlinger hedder begge '{}'. Giv hver sit eget alias, så 'alias/forespørgsel' peger på præcis én forespørgsel.";
    diag_collection_alias_shadows_folder => "The alias '{}' is also a folder in the bound collection, so '{0}/…' could mean either. Rename the alias.", "L'alias '{}' est aussi un dossier de la collection liée : '{0}/…' devient ambigu. Renommez l'alias.", "Aliasset '{}' er også en mappe i den bundne samling, så '{0}/…' kan betyde begge dele. Omdøb aliasset.";
    diag_collection_helper_not_open => "it lives on a git remote, so open it as a collection first", "elle se trouve sur un dépôt git distant : ouvrez-la d'abord comme collection", "den ligger på en git-fjernserver, så åbn den som samling først";
    diag_collection_helper_unreadable => "Helper collection '{}' could not be read: {}", "Impossible de lire la collection d'appoint '{}' : {}", "Hjælpesamlingen '{}' kunne ikke læses: {}";
    diag_request_not_found => "Request '{}' was not found in the bound collection.", "La requête '{}' est introuvable dans la collection liée.", "Forespørgslen '{}' blev ikke fundet i den bundne samling.";
    diag_request_ambiguous_leaf => "Request '{}' is ambiguous — {} requests end with that name; qualify it with its folder path.", "La requête '{}' est ambiguë — {} requêtes se terminent par ce nom\u{a0}; qualifiez-la avec son chemin de dossier.", "Forespørgslen '{}' er tvetydig — {} forespørgsler slutter med det navn; kvalificér den med dens mappesti.";
    diag_envs_empty => "This ENVS loop has no environments to run over.", "Cette boucle ENVS n'a aucun environnement à parcourir.", "Denne ENVS-løkke har ingen miljøer at køre over.";
    diag_baseline_multiple => "At most one BASELINE environment is allowed.", "Un seul environnement BASELINE est autorisé.", "Højst ét BASELINE-miljø er tilladt.";
    diag_comparison_missing => "A BASELINE needs at least one COMPARISON environment to be compared against.", "Une BASELINE a besoin d'au moins un environnement COMPARISON auquel se comparer.", "En BASELINE har brug for mindst ét COMPARISON-miljø at blive sammenlignet med.";
    diag_pattern_before_rest => "The pattern binds {} names before '...' but the producer yields only {}.", "Le motif lie {} noms avant '...' alors que le producteur n'en fournit que {}.", "Mønstret binder {} navne før '...', men produceren giver kun {}.";
    diag_pattern_arity => "The pattern binds {} name(s) but the producer yields {} per item — use '_' to discard one or '...' to absorb the extras.", "Le motif lie {} nom(s) alors que le producteur en fournit {} par élément — utilisez '_' pour en ignorer un ou '...' pour absorber le reste.", "Mønstret binder {} navn(e), men produceren giver {} pr. element — brug '_' til at kassere et eller '...' til at opsamle resten.";
    diag_unknown_list => "Unknown list '{}' — declare it with LIST {} = … before using it.", "Liste '{}' inconnue — déclarez-la avec LIST {} = … avant de l'utiliser.", "Ukendt liste '{}' — erklær den med LIST {} = … før du bruger den.";
    diag_list_arity => "The list elements have inconsistent arity — a mix of scalars and tuples of different sizes.", "Les éléments de la liste ont une arité incohérente — un mélange de scalaires et de tuples de tailles différentes.", "Listens elementer har inkonsistent aritet — en blanding af skalarer og tupler af forskellig størrelse.";
    diag_concat_arity => "The CONCAT inputs have inconsistent arity — every input must yield the same number of values per item.", "Les entrées de CONCAT ont une arité incohérente — chaque entrée doit fournir le même nombre de valeurs par élément.", "CONCAT-inputtene har inkonsistent aritet — hvert input skal give det samme antal værdier pr. element.";
    diag_var_maybe_undefined => "Request '{}' uses {} which may not be set at this point in the flow — add it to the environment, or assign it before this request.", "La requête '{}' utilise {} qui pourrait ne pas être défini à ce stade du flux — ajoutez-le à l'environnement ou affectez-le avant cette requête.", "Forespørgslen '{}' bruger {}, som måske ikke er sat på dette punkt i forløbet — føj den til miljøet, eller tildel den før denne forespørgsel.";
}

impl Strings {
    /// The English strings as a shared static.
    ///
    /// For the places that have no language of their own to consult — the
    /// headless runner, and test fixtures that care about *which* diagnostic
    /// fired rather than how it reads.
    pub fn english() -> &'static Strings {
        static EN: std::sync::OnceLock<Strings> = std::sync::OnceLock::new();
        EN.get_or_init(|| Strings::for_language(&Language::English))
    }
}

/// Fill the `{}` placeholders of a translated template, in order.
///
/// `format!` needs a literal, so a translated string with runtime values in it
/// can't go through it. Extra placeholders are left as-is and extra arguments
/// are ignored, so a mistranslation that drops or adds a `{}` degrades to odd
/// text rather than a panic — a validation message is not worth crashing over.
pub fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    for arg in args {
        match rest.split_once("{}") {
            Some((before, after)) => {
                out.push_str(before);
                out.push_str(arg);
                rest = after;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// A language-independent status / notification message. It stores *what*
/// happened, not the translated text, so [`Status::text`] can render it in the
/// current language — the message re-translates when the language changes.
#[derive(Clone, Debug)]
pub enum Status {
    Saved,
    /// A bulk save wrote this many files -- the answer to "Save all changes"
    /// on the quit dialog, which spans however many collections were holding
    /// edits rather than the single file `Saved` speaks for.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    SavedFiles(usize),
    Loaded,
    Cleared,
    NoResponse,
    NotCollection,
    NotEnvironment,
    /// The active tab isn't a report, so a report-only action (Save Report,
    /// BIND) can't proceed.
    NotReport,
    /// A report was (re)bound to a loaded collection; holds the collection's
    /// display name.
    ReportBound(String),
    /// BIND was invoked with no collections open (nothing to bind to).
    ReportBindNoCollections,
    /// Text was copied to the clipboard (a selection or a whole-panel copy).
    Copied,
    /// The active collection has no remembered git origin, so "Save to Git"
    /// can't be opened.
    NoGitOrigin,
    /// A Postman import converted to Hurl and had to leave something behind;
    /// the details are in `CONVERSION-NOTES.md` in the imported folder.
    PostmanNotes,
    /// A Postman import finished, but this many items could not be fetched.
    /// Reported rather than swallowed: a workspace that silently arrived two
    /// collections short is worse than one that says so.
    PostmanSkipped(usize),
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
    /// A streaming "Run All" started: warns that Hurl's automatic cookie jar
    /// isn't carried between requests in streaming mode (switch to batch mode
    /// in Preferences if the collection relies on that).
    RunAllStreamingCookies,
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
    /// A brand-new (in-memory, not yet written) collection was created inside
    /// a Workspace — names the relative file path so the user knows where the
    /// just-added request landed and that it still needs saving.
    WorkspaceCollectionCreated(String),
    WorkspaceReportCreated(String),
    /// A "New Report" destination was refused because, once symlinks are
    /// resolved, it lands outside the workspace root (holds the relative path
    /// that was attempted). See [`crate::tui::reports`]'s containment guard.
    WorkspaceReportEscaped(String),
    /// A new workspace file was created (graphical front-end).
    WsItemCreated(String),
    /// The destination for a new workspace file resolved outside the root.
    WsItemEscaped(String),
    /// A file of that name is already there.
    WsItemExists(String),
    /// The name typed doesn't say which of the three kinds it should be.
    WsItemUnknownKind(String),
    /// A workspace file or folder was dragged into another folder.
    WsItemMoved(String),
    /// The destination folder already holds something of that name.
    WsItemMoveExists(String),
    /// A folder was dropped on itself or one of its own descendants.
    WsItemMoveIntoItself,
    /// The "New Request" wizard was submitted (F2 / Ctrl+Enter) with an empty
    /// URL, which is the one field a request can't be saved without — the
    /// wizard is kept open (focused on the URL field) instead of silently
    /// discarding everything the user typed.
    NewRequestUrlRequired,
    /// A request was deleted from a collection (`x` / delete). Holds the HTTP
    /// method so the message can name it, and always pairs with the "press u
    /// to restore" hint since deletions are easy to trigger by accident.
    RequestDeleted(String),
    /// A collection tab was closed (`x` / Ctrl+W). Pairs with the "press u to
    /// reopen" hint, mirroring [`Status::RequestDeleted`].
    TabClosed,
    /// A Global Environment was deleted (`x`, or auto-confirmed). Holds its
    /// name and pairs with the "press u to reopen" hint.
    EnvDeleted(String),
    /// A deleted Global Environment was reopened (`u`); names it.
    EnvReopened(String),
    /// A request was moved to another collection file in the workspace. Holds
    /// the HTTP method and the destination file's display name.
    RequestMoved(String, String),
    /// A request was copied to another collection file in the workspace (as
    /// [`Status::RequestMoved`], but the original is left in place).
    RequestCopied(String, String),
    /// A collection save was refused because a `[Multipart]` file field has no
    /// file path: it would serialize to an invalid `file,;` line that
    /// PaperBoy's own parser rejects, so the file couldn't be reloaded. Holds
    /// the request title and the offending field key.
    SaveUnreadableEmptyFile {
        req: String,
        field: String,
    },
    /// A theme was saved (created or updated) from the Theme editor; names it.
    ThemeSaved(String),
    /// A custom theme was deleted from the Theme editor; names it.
    ThemeDeleted(String),
    /// Save was pressed in the Theme editor with an empty name.
    ThemeNameRequired,
    /// Save was pressed in the Theme editor with a name that clashes with a
    /// built-in preset (presets can't be overwritten).
    ThemeNameReserved,
    /// Delete was pressed in the Theme editor on Automatic or a built-in preset.
    ThemeCannotDelete,
    /// The "New theme" popup was confirmed with a name already in use.
    ThemeNameTaken,
    /// An edit was attempted on a read-only preset (or Automatic) in the editor.
    ThemePresetReadonly,
    /// A raw (non-translatable) error detail, shown after a translated prefix.
    Error(String),
    /// A report run finished (synchronously): how many rows it produced and how
    /// many run-level errors were collected (0 → success).
    ReportRunDone {
        rows: usize,
        errors: usize,
    },
    /// A report couldn't be run or exported; holds an already-translated reason
    /// (built from [`Strings`] at the call site).
    ReportRunBlocked(String),
    /// A structural node edit was reverted with Ctrl+Z (node-editor undo).
    ReportNodeUndone(String),
    /// Ctrl+Z was pressed in the node editor with an empty undo stack.
    ReportNodeNothingToUndo(String),
    /// `a` was pressed on the node editor's settings section when every
    /// optional header directive is already present.
    /// Enter was pressed on a settings row whose picker has nothing to offer —
    /// only reachable for `environment:` with no environments loaded.
    ReportSettingNoChoices,
    /// The report source was re-indented.
    ReportReformatted,
    /// Reformat was asked for on a report that is already correctly indented.
    ReportAlreadyTidy,
    /// Reformat declined; holds the reason (a parse error, or the safety check
    /// that compares the re-indented AST with the original's).
    ReportReformatFailed(String),
    /// A report's results were written to a CSV file; holds its path.
    ReportExported(String),
    /// A report's last run was saved as a `.baseline` snapshot; holds its path.
    ReportBaselineSaved(String),
    /// An exported report was handed to the desktop's opener; holds its path.
    ReportOpened(String),
    /// The column picker was opened without a prior run (columns unknown).
    ReportColumnsNeedRun,
    /// The column picker was applied with nothing selected.
    ReportColumnsNoneSelected,
    /// The column picker's selection was written to the flow's `columns:`.
    ReportColumnsApplied,
    /// A report run has been started on a background thread (non-blocking); the
    /// app stays responsive while it runs. Cleared by the completion status.
    ReportRunning,
    /// Live streaming progress of a background report run: how many of the
    /// projected rows have completed so far. Updated as each row streams in.
    ReportRunProgress {
        done: usize,
        total: usize,
    },
    /// A running report was stopped by the user before it finished. Completed
    /// rows are retained in the results grid; unstarted rows remain as skeleton
    /// placeholders. The user can view, save, or export the partial output.
    ReportRunStopped,
    /// The selected request was reverted to its last-saved on-disk version,
    /// discarding its in-memory edits. Holds the HTTP method for the message.
    RequestReverted(String),
    /// A whole workspace collection file was reverted to its on-disk version,
    /// discarding every in-memory edit to it. Holds the file name.
    FileReverted(String),
    /// A Global Environment's unsaved edits were discarded, restoring the
    /// last-saved values (added vars dropped, modified ones restored). Names it.
    EnvReverted(String),
    /// A revert (`Ctrl+R`) had nothing to do: the item has no on-disk version
    /// to revert to (a scratch collection / never-saved env), or no unsaved
    /// changes.
    NothingToRevert,
    /// The Workspace tree's extension filter was toggled (`Ctrl+F`). `true`
    /// shows only the workspace's own file types (`.hurl/.json/.vars/.trail`);
    /// `false` shows every file.
    WorkspaceTreeFilter(bool),
}

impl Status {
    /// Whether this represents a successful outcome (green) vs a problem (red).
    pub fn is_ok(&self) -> bool {
        match self {
            Status::CollectionRunSummary { failed, .. } => *failed == 0,
            Status::ReportRunDone { errors, .. } => *errors == 0,
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
                    | Status::RequestMoved(_, _)
                    | Status::RequestCopied(_, _)
                    | Status::ThemeSaved(_)
                    | Status::ThemeDeleted(_)
                    | Status::EnvReopened(_)
                    | Status::ReportExported(_)
                    | Status::ReportOpened(_)
                    | Status::ReportBaselineSaved(_)
                    | Status::ReportColumnsApplied
                    | Status::ReportBound(_)
                    | Status::ReportNodeUndone(_)
                    | Status::RequestReverted(_)
                    | Status::FileReverted(_)
                    | Status::EnvReverted(_)
                    | Status::WorkspaceTreeFilter(_)
            ),
        }
    }

    /// Render the message in the given language.
    pub fn text(&self, s: &Strings) -> String {
        match self {
            Status::Saved => s.file_saved.to_string(),
            Status::SavedFiles(n) => s.gui_saved_n_files.replace("{n}", &n.to_string()),
            Status::Loaded => s.file_loaded.to_string(),
            Status::Cleared => s.clear_all_done.to_string(),
            Status::Copied => s.copied_to_clipboard.to_string(),
            Status::NoResponse => s.file_no_response.to_string(),
            Status::NotCollection => s.file_not_collection.to_string(),
            Status::NotEnvironment => s.file_not_environment.to_string(),
            Status::NotReport => s.status_not_report.to_string(),
            Status::ReportBound(name) => format!("{} {name}", s.report_bound_status),
            Status::ReportBindNoCollections => s.report_bind_no_collections.to_string(),
            Status::NoGitOrigin => s.git_no_origin.to_string(),
            Status::PostmanNotes => s.postman_notes_written.to_string(),
            Status::PostmanSkipped(n) => format!("{n} {}", s.postman_skipped),
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
            Status::WorkspaceCollectionCreated(name) => {
                s.workspace_collection_created.replace("{name}", name)
            }
            Status::WorkspaceReportCreated(name) => {
                s.workspace_report_created.replace("{name}", name)
            }
            Status::WorkspaceReportEscaped(name) => {
                s.workspace_report_escaped.replace("{name}", name)
            }
            Status::WsItemCreated(name) => s.ws_item_created.replace("{name}", name),
            Status::WsItemEscaped(name) => s.ws_item_escaped.replace("{name}", name),
            Status::WsItemExists(name) => s.ws_item_exists.replace("{name}", name),
            Status::WsItemUnknownKind(name) => s.ws_item_unknown_kind.replace("{name}", name),
            Status::WsItemMoved(name) => s.ws_item_moved.replace("{name}", name),
            Status::WsItemMoveExists(name) => s.ws_item_move_exists.replace("{name}", name),
            Status::WsItemMoveIntoItself => s.ws_item_move_into_itself.to_string(),
            Status::NewRequestUrlRequired => s.new_request_url_required.to_string(),
            Status::RequestDeleted(method) => s.request_deleted.replace("{m}", method),
            Status::TabClosed => s.tab_closed.to_string(),
            Status::EnvDeleted(name) => s.env_deleted.replace("{n}", name),
            Status::EnvReopened(name) => s.env_reopened.replace("{n}", name),
            Status::RequestMoved(method, dest) => s
                .request_moved
                .replace("{m}", method)
                .replace("{dest}", dest),
            Status::RequestCopied(method, dest) => s
                .request_copied
                .replace("{m}", method)
                .replace("{dest}", dest),
            Status::SaveUnreadableEmptyFile { req, field } => s
                .save_unreadable_empty_file
                .replace("{field}", field)
                .replace("{req}", req),
            Status::ThemeSaved(name) => format!("{} {name}", s.theme_saved),
            Status::ThemeDeleted(name) => format!("{} {name}", s.theme_deleted),
            Status::ThemeNameRequired => s.theme_name_required.to_string(),
            Status::ThemeNameReserved => s.theme_name_reserved.to_string(),
            Status::ThemeCannotDelete => s.theme_cannot_delete.to_string(),
            Status::ThemeNameTaken => s.theme_name_taken.to_string(),
            Status::ThemePresetReadonly => s.theme_preset_readonly.to_string(),
            Status::CollectionRunSummary {
                passed,
                failed,
                total,
            } => format!(
                "{}: {passed}  {}: {failed}  {}: {total}",
                s.run_summary_passed, s.run_summary_failed, s.run_summary_total
            ),
            Status::RunAllStreamingCookies => s.run_all_streaming_cookies.to_string(),
            Status::Error(e) => format!("{} {e}", s.file_error_prefix),
            Status::ReportRunDone { rows, errors } => {
                if *errors == 0 {
                    format!("{} {rows} {}", s.report_run_complete, s.report_status_rows)
                } else {
                    format!(
                        "{} {rows} {}, {errors} {}",
                        s.report_run_complete, s.report_status_rows, s.report_status_errors
                    )
                }
            }
            Status::ReportRunBlocked(reason) => reason.clone(),
            Status::ReportNodeUndone(msg) => msg.clone(),
            Status::ReportNodeNothingToUndo(msg) => msg.clone(),
            Status::ReportReformatted => s.report_reformatted.to_string(),
            Status::ReportAlreadyTidy => s.report_already_tidy.to_string(),
            Status::ReportReformatFailed(why) => {
                format!("{} {why}", s.report_reformat_failed_prefix)
            }
            Status::ReportSettingNoChoices => s.report_setting_no_choices.to_string(),
            Status::ReportExported(path) => format!("{} {path}", s.report_exported_prefix),
            Status::ReportOpened(path) => format!("{} {path}", s.report_opened_prefix),
            Status::ReportBaselineSaved(path) => {
                format!("{} {path}", s.report_baseline_saved_prefix)
            }
            Status::ReportColumnsNeedRun => s.report_columns_need_run.to_string(),
            Status::ReportColumnsNoneSelected => s.report_columns_none_selected.to_string(),
            Status::ReportColumnsApplied => s.report_columns_applied.to_string(),
            Status::ReportRunning => s.report_running.to_string(),
            Status::ReportRunProgress { done, total } => s
                .report_running_progress
                .replace("{done}", &done.to_string())
                .replace("{total}", &total.to_string()),
            Status::ReportRunStopped => s.report_run_stopped.to_string(),
            Status::RequestReverted(method) => format!("{method} {}", s.status_request_reverted),
            Status::FileReverted(name) => format!("{} {name}", s.status_file_reverted),
            Status::EnvReverted(name) => format!("{} {name}", s.status_env_reverted),
            Status::NothingToRevert => s.status_nothing_to_revert.to_string(),
            Status::WorkspaceTreeFilter(on) => {
                if *on {
                    s.workspace_tree_filter_on.to_string()
                } else {
                    s.workspace_tree_filter_off.to_string()
                }
            }
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

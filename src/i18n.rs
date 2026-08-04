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
    browser_hint_workspace => "Enter open folder · Space choose as Workspace · ← parent · ^r reset · Esc cancel", "Entrée ouvrir dossier · Espace choisir comme Workspace · ← dossier parent · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Mellemrum vælg som Workspace · ← overordnet · ^r nulstil · Esc annuller";
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
    git_token_label => "Access token (optional)", "Jeton d'accès (facultatif)", "Adgangstoken (valgfrit)";
    git_connect_hint => "Tab switch field · Enter connect · Esc cancel", "Tab changer de champ · Entrée connecter · Échap annuler", "Tab skift felt · Enter forbind · Esc annuller";
    git_recent_hint => "↓ recent URLs · Enter select", "↓ URL récentes · Entrée sélectionner", "↓ seneste URL'er · Enter vælg";
    git_pick_ref_title => "Select a branch or tag", "Sélectionnez une branche ou une étiquette", "Vælg en gren eller et tag";
    git_pick_file_title => "Select a file", "Sélectionnez un fichier", "Vælg en fil";
    git_filter_hint => "Type to filter · ↑↓ move · Enter select · Esc cancel", "Filtrer en tapant · ↑↓ déplacer · Entrée choisir · Échap annuler", "Skriv for at filtrere · ↑↓ flyt · Enter vælg · Esc annuller";
    git_loading_refs => "Fetching branches and tags…", "Récupération des branches et étiquettes…", "Henter grene og tags…";
    git_loading_files => "Fetching file list…", "Récupération de la liste des fichiers…", "Henter filliste…";
    git_loading_file => "Fetching file…", "Récupération du fichier…", "Henter fil…";
    git_loading_workspace_files => "Downloading matching files…", "Téléchargement des fichiers correspondants…", "Henter matchende filer…";
    git_loading_hint => "(Esc to cancel)", "(Échap pour annuler)", "(Esc for at annullere)";
    git_error_hint => "Press Esc to close", "Appuyez sur Échap pour fermer", "Tryk på Esc for at lukke";
    git_url_required => "A Git URL is required.", "Une URL Git est requise.", "En Git-URL er påkrævet.";
    git_branches => "Branches", "Branches", "Grene";
    git_tags => "Tags", "Étiquettes", "Tags";
    git_filter_label => "filter: ", "filtre\u{a0}: ", "filter: ";
    git_pick_workspace_filter_title => "Choose which files to download", "Choisissez les fichiers à télécharger", "Vælg hvilke filer der skal hentes";
    git_workspace_filter_hint => "↑↓ move · Enter select · Esc cancel", "↑↓ déplacer · Entrée choisir · Échap annuler", "↑↓ flyt · Enter vælg · Esc annuller";
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
    save_workspace => "Save Workspace — Choose Destination Folder", "Enregistrer le Workspace — Choisir le dossier de destination", "Gem Workspace — Vælg destinationsmappe";
    save_collection_folder => "Save Collection — Choose Destination Folder", "Enregistrer la collection — Choisir le dossier de destination", "Gem samling — Vælg destinationsmappe";
    browser_hint_collection_save => "Enter open folder · Tab file name · ← parent · ^r reset · Esc cancel", "Entrée ouvrir dossier · Tab nom du fichier · ← dossier parent · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Tab filnavn · ← overordnet · ^r nulstil · Esc annuller";
    browser_hint_new_report => "Enter open folder · Tab name (use sub/name for a new folder) · ^n scratch tab · ← up · Esc cancel", "Entrée ouvrir dossier · Tab nom (sous-dossier/nom pour créer un dossier) · ^n onglet brouillon · ← remonter · Échap annuler", "Enter åbn mappe · Tab navn (undermappe/navn opretter en mappe) · ^n kladdefane · ← op · Esc annuller";
    browser_hint_workspace_save => "Enter open folder · Tab folder name · ← parent · ^r reset · Esc cancel", "Entrée ouvrir dossier · Tab nom du dossier · ← dossier parent · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Tab mappenavn · ← overordnet · ^r nulstil · Esc annuller";
    browser_filename_label => "File name", "Nom du fichier", "Filnavn";
    browser_foldername_label => "Folder name", "Nom du dossier", "Mappenavn";
    browser_name_hint => "Enter save · Esc back to list", "Entrée enregistrer · Échap retour à la liste", "Enter gem · Esc tilbage til listen";
    browser_filter_label => "Filter: ", "Filtre : ", "Filter: ";
    workspace_save_success => "Workspace saved.", "Workspace enregistré.", "Workspace gemt.";
    workspace_save_failed => "Could not save the workspace ({e}).", "Impossible d'enregistrer le Workspace ({e}).", "Kunne ikke gemme workspace ({e}).";
    git_workspace_storage_q => "Workspace downloaded. Keep it in a temporary folder, or save it to a permanent location now?", "Workspace téléchargé. Le garder dans un dossier temporaire, ou l'enregistrer dans un emplacement permanent maintenant ?", "Workspace downloadet. Behold den i en midlertidig mappe, eller gem den på en permanent placering nu?";
    git_workspace_storage_temp => "Keep temporarily", "Garder temporairement", "Behold midlertidigt";
    git_workspace_storage_choose => "Choose a folder…", "Choisir un dossier…", "Vælg en mappe…";
    git_no_origin => "This collection wasn't loaded from Git.", "Cette collection n'a pas été chargée depuis Git.", "Denne samling blev ikke indlæst fra Git.";
    git_save_title => "Save to Git", "Enregistrer sur Git", "Gem til Git";
    git_save_workspace_empty => "The workspace has no files to save.", "Le workspace n'a aucun fichier à enregistrer.", "Dette workspace har ingen filer at gemme.";
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
    git_save_step_hint => "Tab switch field · Enter continue · Esc cancel", "Tab changer de champ · Entrée continuer · Échap annuler", "Tab skift felt · Enter fortsæt · Esc annuller";
    git_save_branch_label => "Branch", "Branche", "Gren";
    git_save_tag_label => "Tag", "Étiquette", "Tag";
    git_save_target_hint => "Tab Branch/Tag · type a name · ↑↓ pick existing · Enter continue · Esc cancel", "Tab Branche/Étiquette · saisir un nom · ↑↓ choisir existant · Entrée continuer · Échap annuler", "Tab Gren/Tag · skriv et navn · ↑↓ vælg eksisterende · Enter fortsæt · Esc annuller";
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
    foot_reload_var => "Reload var", "Recharger var", "Genindlæs var";
    foot_file => "File", "Fichier", "Fil";
    foot_options => "Settings", "Paramètres", "Indstillinger";
    foot_rename => "rename", "renommer", "omdøb";
    foot_close => "delete", "supprimer", "fjern";
    foot_copy_selection => "copy", "copier", "kopiér";
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
    help_grammar_with => "field may alias an intrinsic, add STATISTICS, or be quoted", "un champ peut aliaser un intrinsèque, STATISTICS, ou être cité", "et felt kan aliasere en intrinsic, STATISTICS eller citeres";
    help_grammar_parallel => "prefix a FOR to run its iterations concurrently", "préfixer un FOR pour exécuter ses itérations en parallèle", "sæt foran et FOR for at køre dets gentagelser samtidigt";
    help_grammar_for => "loop over a source, binding VAR each pass; END closes it", "boucler sur une source, en liant VAR à chaque passage ; END la ferme", "gennemløb en kilde og bind VAR hver gang; END lukker den";
    help_grammar_for_tuple => "destructure each tuple into several variables", "décomposer chaque tuple en plusieurs variables", "udpak hver tuple i flere variabler";
    help_grammar_pattern => "'_' skips a position; '...' absorbs the rest", "« _ » ignore une position ; « ... » absorbe le reste", "« _ » springer en position over; « ... » opsamler resten";
    help_grammar_list => "name a source so a loop can reuse it below", "nommer une source pour qu'une boucle la réutilise", "navngiv en kilde, så en løkke kan genbruge den nedenfor";
    help_grammar_list_literal => "an inline list of scalars or (\"a\", \"b\") tuples", "une liste littérale de scalaires ou de tuples (\"a\", \"b\")", "en inline-liste af skalarer eller (\"a\", \"b\")-tupler";
    help_grammar_files => "file paths under a folder (glob *, ** — not regex)", "chemins de fichiers dans un dossier (glob *, ** — pas regex)", "filstier i en mappe (glob *, ** — ikke regex)";
    help_grammar_folders => "subfolders; each role-glob binds one file per folder", "sous-dossiers ; chaque glob de rôle lie un fichier par dossier", "undermapper; hvert rolle-glob binder én fil per mappe";
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
    browser_hint => "Enter open · ← parent · ^h hidden · ^r reset · Esc cancel", "Entrée ouvrir · ← dossier parent · ^h fichiers cachés · ^r réinitialiser · Échap annuler", "Enter åbn · ← overordnet · ^h skjulte · ^r nulstil · Esc annuller";
    browser_hint_filter => "Enter open · Tab all/matching · type to filter · ← parent · ^h hidden · ^r reset · Esc cancel", "Entrée ouvrir · Tab tous/correspondants · taper pour filtrer · ← dossier parent · ^h fichiers cachés · ^r réinitialiser · Échap annuler", "Enter åbn · Tab alle/matchende · skriv for at filtrere · ← overordnet · ^h skjulte · ^r nulstil · Esc annuller";
    tabs_heading => "Collections", "Collections", "Samlinger";
    suggest_hint => "↓↑ select · Enter fill", "↓↑ sélectionner · Entrée remplir", "↓↑ vælg · Enter udfyld";
    report_default_name => "Untitled Report", "Rapport sans titre", "Unavngiven rapport";
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
    report_hint_results => "Enter drill-down · v source · Ctrl+S export · B baseline", "Entrée exploration · v source · Ctrl+S export · B référence", "Enter udforsk · v kilde · Ctrl+S export · B basislinje";
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
    report_baseline_saved_prefix => "Baseline saved to", "Référence enregistrée dans", "Basislinje gemt i";
    report_baseline_no_result => "Run the report before saving a baseline.", "Exécutez le rapport avant d'enregistrer une référence.", "Kør rapporten før du gemmer en basislinje.";
    report_hint_dry => "d dry-run", "d simulation", "d prøvekørsel";
    report_hint_bind => "b bind", "b lier", "b bind";
    report_hint_nodes => "Enter nodes", "Entrée nœuds", "Enter noder";
    report_hint_view => "v output", "v sortie", "v output";
    report_dry_run_title => "Dry run — expansion preview", "Simulation — aperçu de l'expansion", "Prøvekørsel — udvidelsesforhåndsvisning";
    report_dry_run_rows => "Projected rows:", "Lignes prévues :", "Forventede rækker:";
    report_dry_run_no_rows => "No rows would be produced.", "Aucune ligne ne serait produite.", "Ingen rækker ville blive produceret.";
    report_dry_run_problems_heading => "Problems", "Problèmes", "Problemer";
    report_dry_run_no_problems => "No problems found.", "Aucun problème détecté.", "Ingen problemer fundet.";
    report_dry_run_hint => "↑/↓ scroll · Esc close", "↑/↓ défiler · Échap fermer", "↑/↓ rul · Esc luk";
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
    status_env_reverted => "reverted to last saved:", "rétabli à la dernière sauvegarde :", "gendannet til sidst gemte:";
    status_nothing_to_revert => "Nothing to revert (no saved version or no changes)", "Rien à rétablir (aucune version sauvegardée ou aucune modification)", "Intet at gendanne (ingen gemt version eller ingen ændringer)";
    report_running_indicator => "⏳ Running…", "⏳ En cours…", "⏳ Kører…";
    report_nodes_heading => "Structure", "Structure", "Struktur";
    report_nodes_hint => "a add · Enter configure · e edit line · f File · Del remove · Shift+↑/↓ move · Ctrl+Z undo · Esc source", "a ajouter · Entrée configurer · e modifier la ligne · f Fichier · Suppr retirer · Maj+↑/↓ déplacer · Ctrl+Z annuler · Échap source", "a tilføj · Enter konfigurer · e rediger linje · f Fil · Del fjern · Skift+↑/↓ flyt · Ctrl+Z fortryd · Esc kilde";
    report_nodes_parse_error => "Fix the source before editing as nodes", "Corrigez la source avant de modifier en nœuds", "Ret kilden før redigering som noder";
    report_node_begin => "Begin", "Début", "Start";
    node_menu_title => "Add Node", "Ajouter un nœud", "Tilføj node";
    node_menu_hint => "↑/↓ select · Enter add · Esc cancel", "↑/↓ sélectionner · Entrée ajouter · Échap annuler", "↑/↓ vælg · Enter tilføj · Esc annuller";
    node_pick_request_title => "Choose Request", "Choisir une requête", "Vælg forespørgsel";
    node_pick_request_hint => "↑/↓ select · Enter choose · Esc cancel", "↑/↓ sélectionner · Entrée choisir · Échap annuler", "↑/↓ vælg · Enter vælg · Esc annuller";
    node_pick_request_none => "No requests in the bound collection", "Aucune requête dans la collection liée", "Ingen forespørgsler i den bundne samling";
    node_kind_request => "REQUEST — send a request", "REQUEST — envoyer une requête", "REQUEST — send en forespørgsel";
    node_kind_report_request => "REPORT REQUEST — send and report its fields", "REPORT REQUEST — envoyer et rapporter ses champs", "REPORT REQUEST — send og rapportér dens felter";
    node_kind_report_var => "REPORT — report a variable", "REPORT — rapporter une variable", "REPORT — rapportér en variabel";
    node_kind_assign => "SET — assign a variable", "SET — affecter une variable", "SET — tildel en variabel";
    node_kind_for_files => "FOR … IN FILES — loop over files", "FOR … IN FILES — boucler sur des fichiers", "FOR … IN FILES — løkke over filer";
    node_kind_for_folders => "FOR … IN FOLDERS — loop over folders", "FOR … IN FOLDERS — boucler sur des dossiers", "FOR … IN FOLDERS — løkke over mapper";
    node_kind_for_envs => "FOR … IN ENVS — loop over environments", "FOR … IN ENVS — boucler sur des environnements", "FOR … IN ENVS — løkke over miljøer";
    node_kind_list => "LIST — declare a list", "LIST — déclarer une liste", "LIST — erklær en liste";
    report_node_edit_title => "Edit Node Line", "Modifier la ligne du nœud", "Rediger nodelinje";
    report_node_edit_hint => "Enter apply · Esc cancel", "Entrée appliquer · Échap annuler", "Enter anvend · Esc annuller";
    report_node_line_invalid => "Not a valid statement", "Instruction non valide", "Ikke en gyldig sætning";
    report_node_undone => "Undid last node change", "Dernière modification de nœud annulée", "Fortrød sidste nodeændring";
    report_node_undo_empty => "Nothing to undo", "Rien à annuler", "Intet at fortryde";
    report_node_folder_pick => "Choose loop folder", "Choisir le dossier de la boucle", "Vælg løkkemappe";
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
    report_node_envs_hint => "↑↓ move · ←/→ pick env · b baseline · f file · n add · x remove · Enter apply · Esc cancel", "↑↓ déplacer · ←/→ choisir env · b référence · f fichier · n ajouter · x retirer · Entrée appliquer · Échap annuler", "↑↓ flyt · ←/→ vælg miljø · b basislinje · f fil · n tilføj · x fjern · Enter anvend · Esc annuller";
    report_node_envs_var_label => "Loop variable", "Variable de boucle", "Løkkevariabel";
    report_node_envs_mode_label => "Mode", "Mode", "Tilstand";
    report_node_envs_mode_plain => "Iterate", "Itérer", "Iterér";
    report_node_envs_mode_roles => "Compare", "Comparer", "Sammenlign";
    report_node_envs_baseline => "Baseline", "Référence", "Basislinje";
    report_node_envs_comparison => "Comparison", "Comparaison", "Sammenligning";
    report_node_envs_file => "FILE", "FILE", "FILE";
    report_node_envs_none => "no environments loaded — load one to pick", "aucun environnement chargé — en charger un pour choisir", "ingen miljøer indlæst — indlæs et for at vælge";
    report_node_parallel_label => "Run PARALLEL", "Exécuter en PARALLÈLE", "Kør PARALLELT";
    checkbox_checked => "[x]", "[x]", "[x]";
    checkbox_unchecked => "[ ]", "[ ]", "[ ]";
    report_node_files_title => "Configure FILES loop", "Configurer la boucle FILES", "Konfigurer FILES-løkke";
    report_node_files_hint => "↑↓ move · Space folder/parallel · type var/match · Enter apply · Esc cancel", "↑↓ déplacer · Espace dossier/parallèle · saisir var/match · Entrée appliquer · Échap annuler", "↑↓ flyt · Mellemrum mappe/parallel · skriv var/match · Enter anvend · Esc annuller";
    report_node_files_var_label => "Loop variable", "Variable de boucle", "Løkkevariabel";
    report_node_files_folder_label => "Folder", "Dossier", "Mappe";
    report_node_files_match_label => "Match (glob)", "Filtre (glob)", "Match (glob)";
    report_node_files_none => "no folder chosen — Space to pick", "aucun dossier choisi — Espace pour choisir", "ingen mappe valgt — Mellemrum for at vælge";
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
    /// A report's results were written to a CSV file; holds its path.
    ReportExported(String),
    /// A report's last run was saved as a `.baseline` snapshot; holds its path.
    ReportBaselineSaved(String),
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
                    | Status::ReportBaselineSaved(_)
                    | Status::ReportColumnsApplied
                    | Status::ReportBound(_)
                    | Status::ReportNodeUndone(_)
                    | Status::RequestReverted(_)
                    | Status::EnvReverted(_)
                    | Status::WorkspaceTreeFilter(_)
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
            Status::NotReport => s.status_not_report.to_string(),
            Status::ReportBound(name) => format!("{} {name}", s.report_bound_status),
            Status::ReportBindNoCollections => s.report_bind_no_collections.to_string(),
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
            Status::WorkspaceCollectionCreated(name) => {
                s.workspace_collection_created.replace("{name}", name)
            }
            Status::WorkspaceReportCreated(name) => {
                s.workspace_report_created.replace("{name}", name)
            }
            Status::WorkspaceReportEscaped(name) => {
                s.workspace_report_escaped.replace("{name}", name)
            }
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
            Status::ReportExported(path) => format!("{} {path}", s.report_exported_prefix),
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

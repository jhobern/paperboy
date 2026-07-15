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
    no_response_yet => "Run a request to see the response.", "Exécutez une requête pour voir la réponse.", "Kør en anmodning for at se svaret.";
    req_error_prefix => "Request error:", "Erreur de requête :", "Anmodningsfejl:";
    options_menu => "Settings", "Paramètres", "Indstillinger";
    options_menu_label => "(S)ettings", "Paramètre(s)", "Ind(s)tillinger";
    language_label => "Language", "Langue", "Sprog";
    lang_english => "English", "English", "English";
    lang_french => "Français", "Français", "Français";
    lang_danish => "Dansk", "Dansk", "Dansk";
    clear_all => "Close all collections", "Fermer toutes les collections", "Luk alle samlinger";
    clear_all_done => "All collections closed", "Toutes les collections ont été fermées", "Alle samlinger er lukket";
    copied_to_clipboard => "Copied to clipboard", "Copié dans le presse-papiers", "Kopieret til udklipsholder";
    preferences_menu => "Preferences", "Préférences", "Præferencer";
    confirm_on_exit => "Confirm on exit", "Confirmer à la sortie", "Bekræft ved afslutning";
    confirm_exit_secrets => "There are environment secrets with unsaved changes, exiting will cause these changes to be lost.", "Il y a des secrets d'environnement avec des modifications non enregistrées, quitter entraînera la perte de ces modifications.", "Der er miljøhemmeligheder med ikke-gemte ændringer. Hvis du afslutter, vil disse ændringer gå tabt.";
    confirm_on_clear => "Confirm on clear", "Confirmer avant de fermer", "Bekræft ved lukning";
    default_request_view_label => "Default Request View", "Vue de requête par défaut", "Standard anmodningsvisning";
    view_json_label => "JSON", "JSON", "JSON";
    view_hurl_label => "Hurl", "Hurl", "Hurl";
    confirm_exit_q => "Quit PaperBoy?", "Quitter PaperBoy\u{a0}?", "Afslut PaperBoy?";
    confirm_clear_q => "Close all collections? This removes all tabs and requests.", "Fermer toutes les collections\u{a0}? Cela supprime tous les onglets et requêtes.", "Luk alle samlinger? Dette fjerner alle faner og anmodninger.";
    confirm_save_collection_q => "There are {r} new or modified request entries. Saving will overwrite the original collection file. Proceed?", "Il y a {r} requête(s) nouvelle(s) ou modifiée(s). L'enregistrement écrasera le fichier de collection d'origine. Continuer\u{a0}?", "Der er {r} nye eller ændrede anmodninger. Gemning vil overskrive den oprindelige samlingsfil. Fortsæt?";
    confirm_save_env_q => "There are {e} new or modified environment entries. Saving will overwrite the original environment file. Proceed?", "Il y a {e} variable(s) d'environnement nouvelle(s) ou modifiée(s). L'enregistrement écrasera le fichier d'environnement d'origine. Continuer\u{a0}?", "Der er {e} nye eller ændrede miljøvariabler. Gemning vil overskrive den oprindelige miljøfil. Fortsæt?";
    confirm_overwrite_q => "\"{f}\" already exists. Overwrite it?", "«\u{a0}{f}\u{a0}» existe déjà. L'écraser\u{a0}?", "«{f}» findes allerede. Overskriv den?";
    confirm_yes => "Yes", "Oui", "Ja";
    confirm_no => "No", "Non", "Nej";
    file_menu => "File", "Fichier", "Fil";
    file_menu_label => "(F)ile", "(F)ichier", "(F)il";
    file_menu_item_load => "(L)oad", "(C)harger", "(I)ndlæs";
    file_menu_item_save => "(S)ave", "(E)nregistrer", "(G)em";
    file_load_menu => "Load", "Charger", "Indlæs";
    file_save_menu => "Save", "Enregistrer", "Gem";
    file_load_item_request => "(R)equest…", "(R)equête…", "(A)nmodning…";
    file_load_item_collection => "(C)ollection…", "(C)ollection…", "(S)amling…";
    file_load_item_collection_git => "Collection from (G)it…", "Collection depuis (G)it…", "Samling fra (G)it…";
    file_load_item_environment => "(E)nvironment…", "(E)nvironnement…", "(M)iljø…";
    file_load_item_environment_git => "En(v)ironment from Git…", "En(v)ironnement depuis Git…", "Miljø fra Gi(t)…";
    file_load_item_workspace => "(W)orkspace…", "(W)orkspace…", "(W)orkspace…";
    file_load_item_workspace_git => "Work(s)pace from Git…", "Work(s)pace depuis Git…", "Works(p)ace fra Git…";
    file_save_item_request => "(R)equest…", "(R)equête…", "(A)nmodning…";
    file_save_item_collection => "(C)ollection…", "(C)ollection…", "(S)amling…";
    file_save_item_collection_as => "Collection (A)s…", "Collection s(o)us…", "Samling s(o)m…";
    file_save_item_collection_git => "Save Collection to (G)it…", "Enregistrer la collection sur (G)it…", "Gem samling til (G)it…";
    file_save_item_environment => "(E)nvironment…", "(E)nvironnement…", "(M)iljø…";
    file_save_item_environment_as => "En(v)ironment As…", "En(v)ironnement sous…", "Miljø - n(y)t navn…";
    file_save_item_workspace => "(W)orkspace…", "(W)orkspace…", "(W)orkspace…";
    file_save_item_workspace_git => "Save Work(s)pace to Git…", "Enregistrer le work(s)pace sur Git…", "Gem workspace til Gi(t)…";
    file_save_item_response => "Res(p)onse…", "Ré(p)onse…", "S(v)ar…";
    save_request => "Save Request…", "Enregistrer la requête…", "Gem anmodning…";
    load_request => "Load Request…", "Charger une requête…", "Indlæs anmodning…";
    open_collection => "Load Collection…", "Charger une collection…", "Indlæs samling…";
    save_collection => "Save Collection…", "Enregistrer la collection…", "Gem samling…";
    save_environment => "Save Environment…", "Enregistrer l'environnement…", "Gem miljø…";
    save_response => "Save Response…", "Enregistrer la réponse…", "Gem svar…";
    file_saved => "Saved.", "Enregistré.", "Gemt.";
    file_loaded => "Loaded.", "Chargé.", "Indlæst.";
    file_no_response => "No response to save.", "Aucune réponse à enregistrer.", "Intet svar at gemme.";
    file_error_prefix => "Error:", "Erreur :", "Fejl:";
    file_not_collection => "Not a valid collection file (no requests found).", "Fichier de collection invalide (aucune requête trouvée).", "Ikke en gyldig samlingsfil (ingen anmodninger fundet).";
    file_not_environment => "Not a valid environment file (expected KEY=value lines).", "Fichier d'environnement invalide (lignes CLÉ=valeur attendues).", "Ikke en gyldig miljøfil (forventede NØGLE=værdi-linjer).";
    open_workspace => "Choose Workspace Folder…", "Choisir le dossier Workspace…", "Vælg Workspace-mappe…";
    browser_hint_workspace => "Enter open folder · Space choose as Workspace · ← parent · ^r reset · Esc cancel", "Entrée ouvrir dossier · Espace choisir comme Workspace · ← dossier parent · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Mellemrum vælg som Workspace · ← overordnet · ^r nulstil · Esc annuller";
    workspace_empty_state => "No collection — press w.", "Aucune collection — appuyez sur w.", "Ingen samling — tryk w.";
    foot_workspace => "browse workspace", "parcourir workspace", "gennemse workspace";
    workspace_picker_title => "Workspace", "Workspace", "Workspace";
    workspace_picker_hint => "Enter open · n new collection · Tab toggle filter · ↑↓ move · Esc cancel", "Entrée ouvrir · n nouvelle collection · Tab basculer filtre · ↑↓ déplacer · Échap annuler", "Enter åbn · n ny samling · Tab skift filter · ↑↓ flyt · Esc annuller";
    workspace_picker_hint_add => "Enter add request here · n new collection · Tab toggle filter · ↑↓ move · Esc cancel", "Entrée ajouter la requête ici · n nouvelle collection · Tab basculer filtre · ↑↓ déplacer · Échap annuler", "Enter tilføj forespørgsel her · n ny samling · Tab skift filter · ↑↓ flyt · Esc annuller";
    workspace_new_collection_title => "New collection (path relative to workspace)", "Nouvelle collection (chemin relatif au workspace)", "Ny samling (sti relativ til workspace)";
    workspace_collection_created => "New collection '{name}' created — Ctrl+S to save.", "Nouvelle collection « {name} » créée — Ctrl+S pour enregistrer.", "Ny samling '{name}' oprettet — Ctrl+S for at gemme.";
    new_request_url_required => "Can't save: the request needs a URL.", "Impossible d'enregistrer : la requête nécessite une URL.", "Kan ikke gemme: forespørgslen kræver en URL.";
    workspace_filter_on => "Filter: .hurl/.json", "Filtre : .hurl/.json", "Filter: .hurl/.json";
    workspace_filter_off => "Filter: All files", "Filtre : tous les fichiers", "Filter: Alle filer";
    workspace_no_files => "No matching files in this folder.", "Aucun fichier correspondant dans ce dossier.", "Ingen matchende filer i denne mappe.";
    tab_request => "Scratch Space", "Brouillon", "Kladde";
    run_entry => "▶ Run", "▶ Exécuter", "▶ Kør";
    entry_request_json => "Request JSON", "JSON de la requête", "Anmodnings-JSON";
    entry_request_hurl => "Request Hurl", "Requête (Hurl)", "Anmodning (Hurl)";
    entry_raw_hurl => "Raw Mode (Hurl)", "Mode brut (Hurl)", "Rå tilstand (Hurl)";
    entry_raw_json => "Raw Mode (JSON)", "Mode brut (JSON)", "Rå tilstand (JSON)";
    invalid_hurl => "Not valid Hurl (expected exactly one request); edit and try again.", "Hurl invalide (une seule requête attendue) ; modifiez et réessayez.", "Ikke gyldig Hurl (forventede præcis én anmodning); ret og prøv igen.";
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
    field_form => "Form", "Formulaire", "Formular";
    field_body => "Body", "Corps", "Brødtekst";
    field_asserts => "Asserts", "Assertions", "Assertions";
    field_captures => "Captures", "Captures", "Captures";
    tab_all => "All", "Tout", "Alle";
    hdr_key => "Key", "Clé", "Nøgle";
    hdr_value => "Value", "Valeur", "Værdi";
    hdr_description => "Description", "Description", "Beskrivelse";
    hdr_type => "Type", "Type", "Type";
    form_type_text => "Text", "Texte", "Tekst";
    form_type_file => "File", "Fichier", "Fil";
    content_type_hint => "Content-Type", "Type de contenu", "Content-Type";
    content_type_auto => "Auto (detect from extension)", "Auto (détecter depuis l'extension)", "Auto (registrer fra filtype)";
    content_type_auto_placeholder => "Auto", "Auto", "Auto";
    hint_pick_file => "^F browse", "^F parcourir", "^F gennemse";
    hint_delete_row => "^D delete row", "^D supprimer la ligne", "^D slet række";
    hint_toggle_enabled => "^E toggle enabled", "^E activer/désactiver", "^E slå til/fra";
    add_header => "\u{FF0B} Add header", "\u{FF0B} Ajouter un en-tête", "\u{FF0B} Tilføj header";
    add_cookie => "\u{FF0B} Add cookie", "\u{FF0B} Ajouter un cookie", "\u{FF0B} Tilføj cookie";
    add_form_field => "\u{FF0B} Add field", "\u{FF0B} Ajouter un champ", "\u{FF0B} Tilføj felt";
    add_assert => "\u{FF0B} Add assert", "\u{FF0B} Ajouter une assertion", "\u{FF0B} Tilføj assertion";
    add_capture => "\u{FF0B} Add capture", "\u{FF0B} Ajouter une capture", "\u{FF0B} Tilføj capture";
    cap_name => "Name", "Nom", "Navn";
    cap_expr => "Expression", "Expression", "Udtryk";
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
    env_add_var_title => "New environment variable", "Nouvelle variable d'environnement", "Ny miljøvariabel";
    env_var_switch => "switch", "changer", "skift";
    env_still_secret => "Still secret", "Toujours secret", "Stadig hemmelig";
    env_still_secret_hint => "Ctrl+T: toggle still-secret", "Ctrl+T\u{a0}: bascule toujours-secret", "Ctrl+T: skift stadig-hemmelig";
    git_collection_menu => "Load Collection from Git…", "Charger une collection depuis Git…", "Indlæs samling fra Git…";
    git_env_menu => "Load Environment from Git…", "Charger un environnement depuis Git…", "Indlæs miljø fra Git…";
    git_workspace_menu => "Load Workspace from Git…", "Charger un Workspace depuis Git…", "Indlæs Workspace fra Git…";
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
    browser_hint_workspace_save => "Enter open folder · Space choose as destination · ← parent · ^r reset · Esc cancel", "Entrée ouvrir dossier · Espace choisir comme destination · ← dossier parent · ^r réinitialiser · Échap annuler", "Enter åbn mappe · Mellemrum vælg som destination · ← overordnet · ^r nulstil · Esc annuller";
    workspace_save_name_prompt => "Workspace name", "Nom du Workspace", "Workspace-navn";
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
    git_save_include_env_label => "Also save the environment", "Enregistrer aussi l'environnement", "Gem også miljøet";
    git_save_collection_path_label => "Collection path in repo", "Chemin de la collection dans le dépôt", "Samlingens sti i repoet";
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
    hint_edit_base_url => "to edit", "pour modifier", "for at redigere";
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
    help_tab_switch_hint => "Tab / ←→ to switch view", "Tab / ←→ pour changer de vue", "Tab / ←→ for at skifte visning";
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
    help_browser_reset => "reset the file browser to the folder it opened in", "réinitialiser l'explorateur de fichiers au dossier d'ouverture", "nulstil filvælgeren til den mappe, den åbnede i";
    help_prev_next_tab => "previous / next tab", "onglet précédent / suivant", "forrige / næste fane";
    help_rename_close => "rename tab (F2) · delete request / close collection tab", "renommer l'onglet (F2) · supprimer la requête / fermer l'onglet", "omdøb fane (F2) · slet anmodning / luk samlingsfane";
    help_reload_var => "reload a failed environment entry (env var / 1Password / SSM)", "recharger une entrée d'environnement en échec (var d'env / 1Password / SSM)", "genindlæs en mislykket miljøvariabel (miljøvariabel / 1Password / SSM)";
    help_env_activate => "activate / deactivate the selected Global Environment", "activer / désactiver l'environnement global sélectionné", "aktivér / deaktivér det valgte globale miljø";
    help_env_delete => "delete the selected Global Environment (unlinks any collections using it)", "supprimer l'environnement global sélectionné (délie les collections qui l'utilisent)", "slet det valgte globale miljø (fjerner link fra samlinger, der bruger det)";
    help_env_link => "link / unlink a Global Environment to the active collection", "lier / délier un environnement global à la collection active", "link / afkobl et globalt miljø til den aktive samling";
    help_env_view_linked => "view the active collection's linked Global Environment", "afficher l'environnement global lié à la collection active", "vis den aktive samlings tilknyttede globale miljø";
    help_env_rename => "rename the selected Global Environment", "renommer l'environnement global sélectionné", "omdøb det valgte globale miljø";
    help_resize => "shrink / grow response pane", "réduire / agrandir le panneau de réponse", "formindsk / forøg svarpanelet";
    help_resize_width => "grow / shrink left column", "agrandir / réduire la colonne de gauche", "forøg / formindsk venstre kolonne";
    help_tab_manage => "close / reopen collection or workspace tab", "fermer / rouvrir un onglet de collection ou d'espace de travail", "luk / genåbn samlings- eller workspace-fane";
    help_tab_reorder => "reorder tabs", "réorganiser les onglets", "omarranger faner";
    help_restore_request => "restore deleted request (List pane)", "restaurer la requête supprimée (volet Liste)", "gendan slettet anmodning (Liste-rude)";
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
    new_request_hint => "Tab/arrows move · PgUp/PgDn tab · Alt+1-6 jump · ^Enter/F2 create · Esc cancel", "Tab/flèches se déplacer · PgUp/PgDn onglet · Alt+1-6 aller à · ^Entrée/F2 créer · Échap annuler", "Tab/pile flyt · PgUp/PgDn faneblad · Alt+1-6 hop til · ^Enter/F2 opret · Esc annuller";
    edit_request_hint => "Tab/arrows move · PgUp/PgDn tab · Alt+1-6 jump · ^Enter/F2 save · Esc cancel", "Tab/flèches se déplacer · PgUp/PgDn onglet · Alt+1-6 aller à · ^Entrée/F2 enregistrer · Échap annuler", "Tab/pile flyt · PgUp/PgDn faneblad · Alt+1-6 hop til · ^Enter/F2 gem · Esc annuller";
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
    tabs_heading => "Collections", "Collections", "Samlinger";
    suggest_hint => "↓↑ select · Enter fill", "↓↑ sélectionner · Entrée remplir", "↓↑ vælg · Enter udfyld";
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
    /// A brand-new (in-memory, not yet written) collection was created inside
    /// a Workspace — names the relative file path so the user knows where the
    /// just-added request landed and that it still needs saving.
    WorkspaceCollectionCreated(String),
    /// The "New Request" wizard was submitted (F2 / Ctrl+Enter) with an empty
    /// URL, which is the one field a request can't be saved without — the
    /// wizard is kept open (focused on the URL field) instead of silently
    /// discarding everything the user typed.
    NewRequestUrlRequired,
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
            Status::WorkspaceCollectionCreated(name) => {
                s.workspace_collection_created.replace("{name}", name)
            }
            Status::NewRequestUrlRequired => s.new_request_url_required.to_string(),
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

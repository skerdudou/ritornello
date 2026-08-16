# Assistants de déclaration de source — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remplacer la saisie à l'aveugle des racines du plugin `files` par deux assistants qui parcourent d'abord et déclarent ensuite — un pour les volumes de l'appareil, un pour les partages SMB — en rendant le montage invisible à l'utilisateur.

**Architecture:** Trois modules purs neufs (`volumes.rs`, `smb.rs`, dérivation de nom dans `roots.rs`), un module d'état d'assistant (`explore.rs`) que `admin.rs` délègue, et quatre composants de page dont un arbre de choix partagé par les deux popins. Le parcours SMB passe par `smbclient` en espace utilisateur : rien n'est monté tant que l'utilisateur n'a pas confirmé.

**Tech Stack:** Rust (tokio, serde, anyhow), Vue 3 (composition API, templates précompilés), `@ritornello/ui`, vitest, Playwright, `smbclient` (samba 4.19), `cifs-utils`.

**Spec:** `docs/superpowers/specs/2026-08-16-assistants-de-source-design.md`

## Global Constraints

- **`cargo` n'existe que dans WSL.** Toute commande Rust se lance ainsi :
  `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files"`
- **Les commandes npm se lancent depuis la racine du worktree**, jamais depuis `ui/` :
  `npm run test -w ritornello-plugin-files-ui`, `npm run typecheck -w ritornello-plugin-files-ui`.
- **Parité i18n stricte.** `src/locales/en.toml` et `deploy/locales/files/fr.toml` doivent avoir **exactement** le même jeu de clés ; un test Rust le vérifie. Les catalogues sont écrits en Tâche 1 — **aucune autre tâche ne les modifie**.
- **Anti-clé.** Chaque variante d'erreur typée est résolue contre le catalogue anglais réellement embarqué, et un message égal à sa propre clé fait échouer le test.
- **`Display` en anglais** pour les journaux ; le français ne vit que dans les catalogues.
- **Le nom d'une racine est un composant de chemin de montage et un nom de fichier d'identifiants.** `nom_valide` (`^[a-z0-9][a-z0-9-]{0,31}$`) reste l'arbitre unique.
- **Le mot de passe ne passe jamais par `argv`** ni ne ressort jamais de `get_data`.
- **`smbclient` écrit ses erreurs sur les deux flux** : `do_connect: … failed (Error NT_STATUS_…)` sur stderr, `session setup failed: NT_STATUS_LOGON_FAILURE` sur **stdout**. Le classement lit **stdout et stderr réunis**.
- **Le code de sortie de `smbclient` est fiable** : 1 en échec, 0 en succès.
- **Tous les formats de ce plan sont mesurés**, client samba 4.19.5-Ubuntu contre un NAS Synology réel. Les sorties citées dans les fixtures sont captées, pas reconstituées. En particulier :
  - `-L -g` rend `Disk|nom|commentaire`, et le partage administratif porte le type **`IPC|`** — il est donc écarté par le préfixe avant même le filtre sur `$`. Une ligne de bruit `SMB1 disabled -- no workgroup available` termine la sortie sans empêcher `rc=0`.
  - `-D "/Un Dossier À Espaces"` fonctionne, y compris sur un nom portant apostrophe et accents (`Le fabuleux Destin d'Amélie Poulain - BO`). C'est ce qui condamne le `-c 'cd "…"'`, qu'une apostrophe aurait cassé.
  - Les attributs de `ls` peuvent tenir sur **plusieurs lettres** (`DA` autant que `D`).
  - Un dossier inexistant rend `cd \X\: NT_STATUS_OBJECT_NAME_NOT_FOUND` avec `rc=1`. **Ce code ne signifie pas « hôte injoignable »** — le confondre ferait lire « la machine n'a pas répondu » devant un simple chemin périmé.
- Commits en français, style du dépôt : `feat(plugin-files): …`, sujet à l'infinitif ou nominal, sans point final.

---

## Structure des fichiers

| Fichier | Responsabilité | État |
|---|---|---|
| `src/locales/en.toml` | catalogue anglais | modifié (T1) |
| `deploy/locales/files/fr.toml` | catalogue français | modifié (T1) |
| `src/volumes.rs` | analyse de `/proc/mounts`, liste blanche, montage propriétaire | créé (T2) |
| `src/roots.rs` | + `sous_chemin_sur`, + `derive_name` | modifié (T3) |
| `src/smb.rs` | analyse des sorties `smbclient`, classement, appels | créé (T4, T5) |
| `src/explore.rs` | état et opérations des deux assistants | créé (T6) |
| `src/admin.rs` | opérations de source, charge utile | modifié (T7) |
| `src/main.rs` | câblage, sonde de capacité | modifié (T8) |
| `src/lib.rs` | déclaration des modules | modifié (T2, T4, T6) |
| `ui/src/donnees.ts` | types et normalisation | modifié (T9) |
| `ui/src/ChoixDossier.vue` | arbre de choix, partagé par les deux popins | créé (T10) |
| `ui/src/DialogueAppareil.vue` | assistant « dossier de l'appareil » | créé (T11) |
| `ui/src/DialoguePartage.vue` | assistant « partage réseau » | créé (T12) |
| `ui/src/VoletSources.vue` | liste des sources déclarées | créé (T13) |
| `ui/src/VoletRacines.vue` + test | — | **supprimés** (T13) |
| `ui/src/FilesAdmin.vue` | câblage des volets | modifié (T14) |
| `ui/src/VoletParcourir.vue` | harmonisation des libellés | modifié (T14) |
| `docs/plugins.md`, `docs/installation.md` | prérequis paquets | modifiés (T15) |
| `web/app/e2e/serve.mjs`, `files.spec.ts` | parcours de bout en bout | modifiés (T16) |

**Ordre et parallélisme.** T1 en premier, seul. Puis T2, T3, T4+T5 et T15 en parallèle (fichiers disjoints). Puis T6 → T7 → T8, avec T9 en parallèle. Puis T10, puis T11 et T12 en parallèle, puis T13, puis T14. T16 en dernier.

---

## Task 1 : Les catalogues i18n

**Files:**
- Modify: `crates/ritornello-plugin-files/src/locales/en.toml`
- Modify: `deploy/locales/files/fr.toml`

**Interfaces:**
- Consumes: rien.
- Produces: toutes les clés consommées par T2–T16. **Aucune autre tâche n'écrit dans ces deux fichiers.**

- [ ] **Step 1 : Ajouter les clés en anglais**

Ajouter à la fin de `src/locales/en.toml` :

```toml
# --- Sources declarees ---
sources_title = "Sources"
no_sources = "No source declared yet. Add a folder or a network share to get started."
btn_add_device = "Add a folder from this device"
btn_add_to_playlist = "Add to playlist"
btn_remove_source = "Remove this source"
btn_retry_mount = "Retry mount"
mount_error_title = "The last mount attempt failed:"

# --- Assistant : dossier de l'appareil ---
dlg_device_title = "Add a folder from this device"
volumes_label = "Volume"
no_volumes = "No usable volume found."
current_path_label = "Selected folder"
audio_here = "{count} audio files here"
btn_choose_folder = "Use this folder"
btn_up = "Up one level"
btn_cancel = "Cancel"

# --- Assistant : partage reseau ---
dlg_share_title = "Add a network share"
btn_connect = "Connect"
connecting = "Connecting…"
shares_label = "Share"
no_shares = "This host exposes no usable share."
btn_manual = "Enter details manually"
btn_assistant = "Back to the wizard"
smb_unavailable = "The network wizard needs the smbclient package. Without it a share can still be declared by entering its details manually."

# --- Refus smbclient ---
smb_not_installed = "smbclient is not installed: install the smbclient package to browse a network share."
smb_bad_credentials = "The host refused these credentials. Check the username and password."
smb_access_denied = "The host accepted the connection but denied access. This account may not be allowed on this share."
smb_unreachable = "Host {host} did not answer. Check the address and that the machine is on."
smb_not_found = "This share or folder no longer exists on the host."
smb_timeout = "Host {host} took too long to answer."
smb_unreadable_output = "smbclient answered something this version cannot read. Raw output: {detail}"

# --- Refus de source ---
bad_local_path = "This path cannot be browsed: {path}"
unknown_source = "Unknown source: {name}"
duplicate_source = "This folder is already declared as a source."
```

- [ ] **Step 2 : Ajouter les mêmes clés en français**

Ajouter à la fin de `deploy/locales/files/fr.toml`, **dans le même ordre** :

```toml
# --- Sources declarees ---
sources_title = "Sources"
no_sources = "Aucune source déclarée. Ajoutez un dossier ou un partage réseau pour commencer."
btn_add_device = "Ajouter un dossier de l'appareil"
btn_add_to_playlist = "Ajouter à la liste"
btn_remove_source = "Retirer cette source"
btn_retry_mount = "Réessayer le montage"
mount_error_title = "La dernière tentative de montage a échoué :"

# --- Assistant : dossier de l'appareil ---
dlg_device_title = "Ajouter un dossier de l'appareil"
volumes_label = "Volume"
no_volumes = "Aucun volume exploitable trouvé."
current_path_label = "Dossier choisi"
audio_here = "{count} fichiers audio ici"
btn_choose_folder = "Choisir ce dossier"
btn_up = "Remonter d'un niveau"
btn_cancel = "Annuler"

# --- Assistant : partage reseau ---
dlg_share_title = "Ajouter un partage réseau"
btn_connect = "Se connecter"
connecting = "Connexion…"
shares_label = "Partage"
no_shares = "Cet hôte n'expose aucun partage exploitable."
btn_manual = "Saisir les informations à la main"
btn_assistant = "Revenir à l'assistant"
smb_unavailable = "L'assistant réseau demande le paquet smbclient. Sans lui, un partage se déclare encore en saisissant ses informations à la main."

# --- Refus smbclient ---
smb_not_installed = "smbclient n'est pas installé : installez le paquet smbclient pour parcourir un partage réseau."
smb_bad_credentials = "L'hôte a refusé ces identifiants. Vérifiez le nom d'utilisateur et le mot de passe."
smb_access_denied = "L'hôte a accepté la connexion mais refusé l'accès. Ce compte n'a peut-être pas le droit sur ce partage."
smb_unreachable = "L'hôte {host} n'a pas répondu. Vérifiez l'adresse et que la machine est allumée."
smb_not_found = "Ce partage ou ce dossier n'existe plus sur l'hôte."
smb_timeout = "L'hôte {host} a mis trop longtemps à répondre."
smb_unreadable_output = "smbclient a répondu quelque chose que cette version ne sait pas lire. Sortie brute : {detail}"

# --- Refus de source ---
bad_local_path = "Ce chemin n'est pas parcourable : {path}"
unknown_source = "Source inconnue : {name}"
duplicate_source = "Ce dossier est déjà déclaré comme source."
```

- [ ] **Step 3 : Retirer les clés devenues mortes**

Retirer des **deux** fichiers : `roots_title`, `ph_root_name`, `btn_add_local`, `btn_save_roots`, `btn_mount_now`.

**Ne pas retirer** : `no_roots` (encore employée par `VoletParcourir`), `root_label`, `password_kept_hint`, `bad_root_name`, `duplicate_root`, `bad_host`, `bad_share`, `bad_subpath`, `relative_local_path` — toutes encore résolues par `RootError::message`.

- [ ] **Step 4 : Vérifier la parité**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files parite"`
Expected: PASS. Un échec nomme la clé en trop d'un côté.

- [ ] **Step 5 : Commit**

```bash
git add crates/ritornello-plugin-files/src/locales/en.toml deploy/locales/files/fr.toml
git commit -m "feat(plugin-files): les libelles des deux assistants de source"
```

---

## Task 2 : `volumes.rs` — les volumes de l'appareil

**Files:**
- Create: `crates/ritornello-plugin-files/src/volumes.rs`
- Modify: `crates/ritornello-plugin-files/src/lib.rs`

**Interfaces:**
- Consumes: rien.
- Produces:
  - `pub struct Volume { pub path: PathBuf, pub fstype: String }` (Serialize)
  - `pub fn volumes(proc_mounts: &str) -> Vec<Volume>`
  - `pub fn proprietaire(proc_mounts: &str, chemin: &Path) -> Option<Volume>`
  - `pub fn parcourable(proc_mounts: &str, chemin: &Path) -> bool`
  - `pub fn lire_proc_mounts() -> String`

- [ ] **Step 1 : Déclarer le module**

Dans `src/lib.rs`, ajouter `pub mod volumes;` dans la liste des modules (ordre alphabétique).

- [ ] **Step 2 : Écrire les tests qui échouent**

Créer `src/volumes.rs` avec **uniquement** le bloc de tests ci-dessous, précédé de `use std::path::{Path, PathBuf};` :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Un /proc/mounts réaliste de Raspberry Pi : la racine, la partition de
    /// démarrage, une clé USB, et les pseudo-systèmes de fichiers qui doivent
    /// rester invisibles.
    const MOUNTS: &str = "\
proc /proc proc rw,relatime 0 0
sysfs /sys sysfs rw,relatime 0 0
/dev/mmcblk0p2 / ext4 rw,relatime 0 0
devtmpfs /dev devtmpfs rw 0 0
tmpfs /run tmpfs rw,nosuid 0 0
/dev/mmcblk0p1 /boot/firmware vfat rw,relatime 0 0
/dev/sda1 /media/ma\\040cle exfat rw,relatime 0 0
//192.168.1.20/musique /mnt/ritornello/nas cifs ro,relatime 0 0
";

    #[test]
    fn seuls_les_vrais_systemes_de_fichiers_sont_proposes() {
        // Liste blanche et non liste noire : une liste noire oublierait le
        // prochain pseudo-système de fichiers du noyau, et l'oubli se verrait
        // seulement sous la forme d'un volume parasite dans une liste de choix.
        let v: Vec<String> = volumes(MOUNTS).iter().map(|v| v.path.display().to_string()).collect();
        assert_eq!(v, vec!["/", "/boot/firmware", "/media/ma cle", "/mnt/ritornello/nas"]);
    }

    #[test]
    fn un_point_de_montage_avec_espace_echappe_est_deechappe() {
        // /proc/mounts échappe l'espace en \040. Sans ce traitement, la clé
        // « ma cle » serait proposée sous un nom que le système de fichiers ne
        // connaît pas, et le parcours échouerait à l'ouverture.
        assert!(volumes(MOUNTS).iter().any(|v| v.path == PathBuf::from("/media/ma cle")));
    }

    #[test]
    fn le_montage_proprietaire_est_le_plus_long_prefixe() {
        // LA règle qui rend la garde correcte. Un test naïf « commence par un
        // volume » accepterait /proc/self/root, puisque /proc commence par /,
        // qui est un volume.
        let p = proprietaire(MOUNTS, Path::new("/boot/firmware/config.txt")).unwrap();
        assert_eq!(p.path, PathBuf::from("/boot/firmware"));
        let p = proprietaire(MOUNTS, Path::new("/home/pi/musique")).unwrap();
        assert_eq!(p.path, PathBuf::from("/"));
    }

    #[test]
    fn les_pseudo_systemes_de_fichiers_ne_sont_pas_parcourables() {
        // Pas pour le secret — ils sont lisibles de toute façon — mais parce
        // qu'un « tout ajouter » lancé sur /proc partirait dans les liens
        // récursifs de /proc/self.
        assert!(!parcourable(MOUNTS, Path::new("/proc/self")));
        assert!(!parcourable(MOUNTS, Path::new("/sys/class")));
        assert!(!parcourable(MOUNTS, Path::new("/run/user/1000")));
        assert!(!parcourable(MOUNTS, Path::new("/dev/shm")));
    }

    #[test]
    fn un_chemin_sous_un_vrai_volume_est_parcourable() {
        assert!(parcourable(MOUNTS, Path::new("/media/ma cle/Albums")));
        assert!(parcourable(MOUNTS, Path::new("/home/pi/musique")));
        assert!(parcourable(MOUNTS, Path::new("/")));
    }

    #[test]
    fn un_surmontage_est_celui_qui_compte() {
        // Deux montages au même endroit : c'est le dernier qui est visible,
        // comme pour le noyau. Se tromper ici ferait déclarer parcourable un
        // chemin que le tmpfs a recouvert.
        let m = "/dev/sda1 /media/x ext4 rw 0 0\ntmpfs /media/x tmpfs rw 0 0\n";
        assert_eq!(proprietaire(m, Path::new("/media/x/a")).unwrap().fstype, "tmpfs");
        assert!(!parcourable(m, Path::new("/media/x/a")));
    }

    #[test]
    fn une_ligne_tronquee_est_ignoree_sans_paniquer() {
        // /proc/mounts est lu à chaud : une ligne partielle ne doit pas faire
        // tomber la page entière.
        assert!(volumes("/dev/sda1\n\n/dev/sdb1 /media/y\n").is_empty());
    }
}
```

- [ ] **Step 3 : Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files volumes"`
Expected: FAIL à la compilation — `cannot find function 'volumes'`.

- [ ] **Step 4 : Écrire l'implémentation**

Insérer **au-dessus** du bloc de tests :

```rust
//! Les volumes montés de l'appareil : ce qu'un assistant peut proposer de
//! parcourir, et ce qu'il doit refuser.
//!
//! Tout est pur et prend le texte de `/proc/mounts` plutôt que de le lire :
//! c'est ce qui permet d'éprouver la garde de parcours sans monter quoi que ce
//! soit, ce qu'un test ne pourrait pas faire sans privilège.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Types de systèmes de fichiers réputés porter des fichiers de l'utilisateur.
///
/// **Liste blanche et non liste noire.** Une liste noire oublierait le prochain
/// pseudo-système de fichiers que le noyau inventera, et cet oubli ne se
/// verrait pas : il se traduirait par un volume parasite dans une liste de
/// choix, ou par un balayage récursif parti dans `/proc`.
const FS_REELS: &[&str] = &[
    "ext2", "ext3", "ext4", "vfat", "exfat", "ntfs", "ntfs3", "btrfs", "xfs", "f2fs", "iso9660",
    "udf", "hfsplus", "cifs", "nfs", "nfs4",
];

const PROC_MOUNTS: &str = "/proc/mounts";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Volume {
    pub path: PathBuf,
    pub fstype: String,
}

/// Déséchappe un champ de `/proc/mounts` : l'espace y est écrit `\040` et la
/// tabulation `\011`.
fn desechappe(s: &str) -> String {
    s.replace("\\040", " ").replace("\\011", "\t")
}

/// Tous les montages, **pseudo-systèmes de fichiers compris**.
///
/// La garde de parcours en a besoin entiers : c'est en connaissant le montage
/// de `/proc` qu'on peut refuser `/proc/self`.
fn tous(proc_mounts: &str) -> Vec<Volume> {
    proc_mounts
        .lines()
        .filter_map(|l| {
            let mut c = l.split_whitespace();
            let _source = c.next()?;
            let point = c.next()?;
            let fstype = c.next()?;
            Some(Volume { path: PathBuf::from(desechappe(point)), fstype: fstype.to_string() })
        })
        .collect()
}

/// Volumes proposables à l'utilisateur, triés.
pub fn volumes(proc_mounts: &str) -> Vec<Volume> {
    let mut retenus: Vec<Volume> = Vec::new();
    for v in tous(proc_mounts) {
        if !FS_REELS.contains(&v.fstype.as_str()) {
            continue;
        }
        // Un même point monté deux fois n'apparaît qu'une fois, et c'est le
        // dernier montage qui compte — comme pour le noyau.
        match retenus.iter_mut().find(|r| r.path == v.path) {
            Some(place) => *place = v,
            None => retenus.push(v),
        }
    }
    retenus.sort_by(|a, b| a.path.cmp(&b.path));
    retenus
}

/// Le montage **propriétaire** d'un chemin : le point de montage le plus long
/// qui le préfixe.
///
/// C'est la seule formulation correcte. Un test « le chemin commence par un
/// volume » accepterait `/proc/self/root`, puisque `/proc` commence par `/`,
/// qui est bien un volume.
///
/// À égalité de longueur, `max_by_key` rend le **dernier** élément, ce qui est
/// exactement la sémantique du surmontage : le dernier monté est celui qu'on
/// voit.
pub fn proprietaire(proc_mounts: &str, chemin: &Path) -> Option<Volume> {
    tous(proc_mounts)
        .into_iter()
        .filter(|v| chemin.starts_with(&v.path))
        .max_by_key(|v| v.path.as_os_str().len())
}

/// Vrai si `chemin` peut être parcouru : son montage propriétaire porte un vrai
/// système de fichiers.
pub fn parcourable(proc_mounts: &str, chemin: &Path) -> bool {
    proprietaire(proc_mounts, chemin)
        .map(|v| FS_REELS.contains(&v.fstype.as_str()))
        .unwrap_or(false)
}

/// Contenu de `/proc/mounts`.
///
/// Le chemin est surchargeable par `RITORNELLO_FILES_PROC_MOUNTS` : c'est ce
/// qui permet au parcours de bout en bout de décrire des volumes sans en
/// monter, sur une machine où le test n'a aucun privilège.
pub fn lire_proc_mounts() -> String {
    let chemin = std::env::var("RITORNELLO_FILES_PROC_MOUNTS")
        .unwrap_or_else(|_| PROC_MOUNTS.to_string());
    std::fs::read_to_string(chemin).unwrap_or_default()
}
```

- [ ] **Step 5 : Lancer les tests**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files volumes"`
Expected: PASS, 7 tests.

- [ ] **Step 6 : Commit**

```bash
git add crates/ritornello-plugin-files/src/volumes.rs crates/ritornello-plugin-files/src/lib.rs
git commit -m "feat(plugin-files): les volumes de l appareil, et le montage proprietaire d un chemin"
```

---

## Task 3 : `roots.rs` — sous-chemin honnête et nom dérivé

**Files:**
- Modify: `crates/ritornello-plugin-files/src/roots.rs`

**Interfaces:**
- Consumes: `nom_valide` (privé, même module).
- Produces:
  - `pub fn derive_name(indice: &str, pris: &[&str]) -> String`
  - `pub fn champ_sur(valeur: &str) -> bool` (passe de privé à public — `smb.rs` en a besoin pour valider l'hôte avant de le poser dans une ligne de commande)

- [ ] **Step 1 : Écrire les tests qui échouent**

Ajouter dans le `mod tests` existant de `src/roots.rs` :

```rust
#[test]
fn un_sous_chemin_a_espaces_est_accepte() {
    // Le défaut corrigé. `champ_sur` refuse l'espace parce que ses valeurs
    // atterrissent dans la ligne d'options de mount.cifs, séparée par des
    // virgules. Un sous-chemin n'y entre JAMAIS : `mount_command` ne pose que
    // l'hôte, le partage et `mount_point()`, qui l'ignore. Lui appliquer la
    // même règle rendait « Ma Musique » indéclarable pour une raison qui ne le
    // concerne pas — et l'assistant propose désormais n'importe quel dossier.
    let r = roots_avec(Root { subpath: Some("Ma Musique/Jazz, live".into()), ..racine_smb() });
    assert!(r.validate().is_ok(), "{:?}", r.validate());
}

#[test]
fn un_sous_chemin_qui_remonte_reste_refuse() {
    for mauvais in ["../../etc", "/etc", "a/../../b", "a//b", "a/./b", "a\0b", ""] {
        let r = roots_avec(Root { subpath: Some(mauvais.into()), ..racine_smb() });
        assert!(
            matches!(r.validate(), Err(RootError::BadSubpath { .. })),
            "accepte a tort : {mauvais:?}"
        );
    }
}

#[test]
fn un_nom_derive_est_toujours_accepte_par_la_grammaire() {
    // L'invariant qui compte : ce nom devient un composant du chemin de
    // montage ET un nom de fichier d'identifiants. La dérivation doit produire
    // du valide par construction, jamais par chance — l'utilisateur ne voit
    // plus ce nom et n'aurait aucun moyen de corriger un refus.
    let hostiles = [
        "../etc", "Ma Musique", "Éric's Jazz!", "///", "", "$$$", "3615",
        "CamelCase", "a b c d e f g h i j k l m n o p q r s t u v w x y z 0 1 2 3",
        "日本語", "-début-tiret-", "fin-tiret---",
    ];
    for h in hostiles {
        let n = derive_name(h, &[]);
        assert!(nom_valide(&n), "indice {h:?} a donne un nom refuse : {n:?}");
    }
}

#[test]
fn deux_indices_identiques_donnent_deux_noms_distincts() {
    // Sans dédoublonnage, la deuxième source écraserait le fichier
    // d'identifiants de la première et se disputerait son point de montage.
    let a = derive_name("Musique", &[]);
    let b = derive_name("Musique", &[a.as_str()]);
    let c = derive_name("Musique", &[a.as_str(), b.as_str()]);
    assert_eq!(a, "musique");
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert!(nom_valide(&b) && nom_valide(&c));
}

#[test]
fn un_indice_tres_long_reste_dedoublonnable() {
    // Le suffixe doit rentrer dans les 32 caractères : le concaténer sans
    // tronquer d'abord produirait un nom refusé, donc une source impossible à
    // déclarer une deuxième fois.
    let long = "a".repeat(60);
    let a = derive_name(&long, &[]);
    let b = derive_name(&long, &[a.as_str()]);
    assert!(nom_valide(&a) && nom_valide(&b), "{a:?} / {b:?}");
    assert_ne!(a, b);
}

#[test]
fn les_accents_se_replient_au_lieu_de_disparaitre() {
    // « Été » qui deviendrait « t » serait un nom exact mais illisible dans les
    // journaux et dans /mnt/ritornello.
    assert_eq!(derive_name("Été à Nîmes", &[]), "ete-a-nimes");
}
```

- [ ] **Step 2 : Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files roots"`
Expected: FAIL — `cannot find function 'derive_name'`.

- [ ] **Step 3 : Rendre `champ_sur` public et ajouter `sous_chemin_sur`**

Remplacer la signature de `champ_sur` par `pub fn champ_sur(valeur: &str) -> bool` (le corps ne change pas), et ajouter juste en dessous :

```rust
/// Grammaire d'un **sous-chemin** parcouru sous un point de montage.
///
/// Distincte de `champ_sur`, et c'est délibéré. `champ_sur` refuse la virgule
/// et l'espace parce que ses valeurs atterrissent dans la ligne d'options de
/// `mount.cifs`, qui les sépare par des virgules. **Un sous-chemin n'y entre
/// jamais** : `mount_command` ne pose que l'hôte, le partage et
/// `mount_point()`, lequel ignore le sous-chemin.
///
/// Leur appliquer la même règle rendait « Ma Musique » indéclarable pour une
/// raison qui ne la concerne pas. Le défaut se voyait peu tant qu'on saisissait
/// le sous-chemin à la main ; il deviendrait constant avec un assistant qui
/// propose de choisir n'importe quel dossier d'un NAS.
fn sous_chemin_sur(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('/')
        && !s.contains('\0')
        && s.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}
```

- [ ] **Step 4 : Brancher `sous_chemin_sur` dans la validation**

Dans `Roots::validate`, remplacer le bloc du sous-chemin par :

```rust
if let Some(s) = &r.subpath {
    if !sous_chemin_sur(s) {
        return Err(RootError::BadSubpath { subpath: s.clone() });
    }
}
```

- [ ] **Step 5 : Écrire la dérivation de nom**

Ajouter à la fin de `src/roots.rs`, **avant** le `mod tests` :

```rust
/// Replie un libellé quelconque en un nom de racine conforme à `nom_valide`.
///
/// L'utilisateur ne saisit plus ce nom : les assistants le dérivent du nom du
/// partage ou du dernier segment du chemin choisi. Comme il devient **un
/// composant du chemin de montage et un nom de fichier d'identifiants**, la
/// dérivation doit produire du valide par construction — un refus après
/// dérivation serait un défaut que rien dans l'IHM ne permettrait de corriger.
///
/// `pris` porte les noms déjà employés : sans dédoublonnage, une deuxième
/// source écraserait le fichier d'identifiants de la première et se disputerait
/// son point de montage.
pub fn derive_name(indice: &str, pris: &[&str]) -> String {
    let base = replie(indice);
    if !pris.contains(&base.as_str()) {
        return base;
    }
    for n in 2..1000 {
        let suffixe = format!("-{n}");
        // Tronquer **avant** de concaténer : ajouter le suffixe à un nom déjà
        // long produirait un nom refusé, donc une source impossible à déclarer
        // une deuxième fois.
        let tete: String = base.chars().take(32 - suffixe.len()).collect();
        let candidat = format!("{}{suffixe}", tete.trim_end_matches('-'));
        if !pris.contains(&candidat.as_str()) {
            return candidat;
        }
    }
    base
}

/// Le repliage lui-même : minuscules ASCII, tiret pour tout le reste.
///
/// Le premier caractère est alphanumérique **par construction** — on ne pousse
/// jamais de tiret sur une chaîne vide — ce qui satisfait la première règle de
/// `nom_valide` sans avoir à la vérifier après coup.
fn replie(indice: &str) -> String {
    let mut out = String::new();
    let mut tiret = false;
    for c in indice.chars() {
        let c = sans_accent(c);
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            tiret = false;
        } else if !out.is_empty() && !tiret {
            out.push('-');
            tiret = true;
        }
    }
    let tronque: String = out.chars().take(32).collect();
    let net = tronque.trim_end_matches('-').to_string();
    // Un indice entièrement non-ASCII ne laisse rien : mieux vaut un nom
    // générique qu'une source impossible à déclarer.
    if net.is_empty() {
        "source".to_string()
    } else {
        net
    }
}

/// Replie les accents latins courants.
///
/// Une table plutôt qu'une caisse de normalisation Unicode : quinze lignes
/// couvrent le français, l'espagnol et l'allemand, et tout le reste tombe de
/// toute façon sur le tiret. « Été » qui deviendrait « t » serait un nom exact
/// mais illisible dans les journaux et sous `/mnt/ritornello`.
fn sans_accent(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ý' | 'ÿ' => 'y',
        'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => 'A',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
        'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' => 'O',
        'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
        'Ç' => 'C',
        'Ñ' => 'N',
        'Ý' => 'Y',
        _ => c,
    }
}
```

- [ ] **Step 6 : Lancer les tests**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files"`
Expected: PASS. Le test existant `un_sous_chemin_qui_remonte_ou_qui_est_absolu_est_refuse` doit toujours passer.

- [ ] **Step 7 : Commit**

```bash
git add crates/ritornello-plugin-files/src/roots.rs
git commit -m "feat(plugin-files): le sous-chemin cesse de mentir sur les espaces, et le nom se derive"
```

---

## Task 4 : `smb.rs` — analyse des sorties, sans processus

**Files:**
- Create: `crates/ritornello-plugin-files/src/smb.rs`
- Modify: `crates/ritornello-plugin-files/src/lib.rs`

**Interfaces:**
- Consumes: `ritornello_i18n::Catalog`.
- Produces:
  - `pub enum SmbError { NotInstalled, BadCredentials, AccessDenied, Unreachable, Timeout, UnreadableOutput(String), Other(String) }`
  - `impl SmbError { pub fn message(&self, catalog: &Catalog, host: &str) -> String }`
  - `pub struct SmbEntry { pub name: String, pub dir: bool }` (Serialize)
  - `pub fn classify(sorties: &str) -> SmbError`
  - `pub fn parse_shares(stdout: &str) -> Vec<String>`
  - `pub fn parse_ls(stdout: &str) -> Result<Vec<SmbEntry>, SmbError>`

- [ ] **Step 1 : Déclarer le module**

Dans `src/lib.rs`, ajouter `pub mod smb;`.

- [ ] **Step 2 : Écrire les tests qui échouent**

Créer `src/smb.rs` avec, pour l'instant, seulement ce bloc :

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_i18n::Catalog;
    use std::path::Path;

    /// Sortie **captée** de `smbclient -L //192.168.1.15 -g` contre un NAS
    /// Synology réel.
    ///
    /// Deux détails qu'on n'aurait pas inventés : le partage administratif
    /// porte le type `IPC|` et non `Disk|`, et une ligne de bruit termine la
    /// sortie sans empêcher un code de retour nul.
    const SHARES: &str = "\
Disk|book|eBooks
Disk|downloads|
Disk|music|System default shared folder
Disk|photo|System default shared folder
Disk|home|Home directory of ritornello
IPC|IPC$|IPC Service ()
SMB1 disabled -- no workgroup available
";

    /// Sortie **captée** de `smbclient //hôte/music -D / -c ls`.
    ///
    /// Les colonnes sont alignées par des espaces : le nom peut donc en
    /// contenir, et l'analyse doit se faire **par la droite**. Les attributs
    /// tiennent sur une ou deux lettres. La dernière entrée est le cas qui
    /// justifie tout : espaces, apostrophe, accents et tiret dans un seul nom.
    const LS: &str = "\
  .                                  DA        0  Fri Apr 17 14:46:30 2026
  ..                                  D        0  Sun Aug 16 16:23:48 2026
  Within Temptation                   D        0  Tue Mar 27 20:20:11 2018
  Eagles Of Death Metal               D        0  Fri Feb  7 16:19:36 2020
  Yann Tiersen                       DA        0  Tue Jul 17 23:07:00 2018
  .cache                             DH        0  Sat Jan  4 11:02:10 2025
  cover.jpg                           A   123456  Sat Jan  4 11:02:10 2025
  piste.mp3                           A  9876543  Sat Jan  4 11:02:10 2025
  Le fabuleux Destin d'Amélie Poulain - BO      D        0  Fri Dec 29 19:49:47 2023

\t\t102400 blocks of size 1024. 102380 blocks available
";

    fn catalogue() -> Catalog {
        Catalog::load("files", "en", Path::new("/inexistant"), crate::FILES_EN)
    }

    #[test]
    fn les_partages_administratifs_sont_ecartes() {
        // Le NAS annonce `IPC$` avec le type `IPC|`, pas `Disk|` : le préfixe
        // l'écarte donc déjà. Le filtre sur `$` reste la ceinture, pour un
        // serveur qui n'aurait pas cette délicatesse.
        assert_eq!(parse_shares(SHARES), vec!["book", "downloads", "home", "music", "photo"]);
    }

    #[test]
    fn la_ligne_de_bruit_finale_nest_pas_un_partage() {
        // « SMB1 disabled -- no workgroup available » termine la sortie d'un
        // NAS moderne sans empêcher un code de retour nul.
        assert!(!parse_shares(SHARES).iter().any(|s| s.contains("SMB1")));
    }

    #[test]
    fn une_sortie_de_partages_vide_ne_panique_pas() {
        assert!(parse_shares("").is_empty());
        assert!(parse_shares("SMB1 disabled -- no workgroup available\n").is_empty());
    }

    #[test]
    fn un_nom_a_espaces_survit_a_l_analyse() {
        // LE piège du format `ls` : les colonnes sont alignées par des espaces.
        // Lire par la gauche casserait sur presque tous les noms d'albums —
        // ceux-ci sont réels.
        let e = parse_ls(LS).unwrap();
        let noms: Vec<&str> = e.iter().map(|x| x.name.as_str()).collect();
        assert!(noms.contains(&"Within Temptation"), "{noms:?}");
        assert!(noms.contains(&"Eagles Of Death Metal"), "{noms:?}");
        // Espaces, apostrophe, accents et tiret dans un seul nom : le cas qui
        // condamne l'analyse par la gauche autant que le `cd "…"` cité.
        assert!(noms.contains(&"Le fabuleux Destin d'Amélie Poulain - BO"), "{noms:?}");
    }

    #[test]
    fn un_jour_du_mois_sur_un_chiffre_ne_decale_pas_le_decoupage() {
        // « Fri Feb  7 » porte deux espaces là où « Fri Feb 17 » n'en a qu'un.
        // Compter les mots sans retrimer décalerait le nom d'une colonne.
        let e = parse_ls(LS).unwrap();
        assert!(e.iter().any(|x| x.name == "Eagles Of Death Metal"));
    }

    #[test]
    fn les_dossiers_se_distinguent_des_fichiers() {
        // Les attributs tiennent sur une ou deux lettres : `D` comme `DA`.
        let e = parse_ls(LS).unwrap();
        assert!(e.iter().find(|x| x.name == "Within Temptation").unwrap().dir);
        assert!(e.iter().find(|x| x.name == "Yann Tiersen").unwrap().dir, "attributs DA");
        assert!(!e.iter().find(|x| x.name == "cover.jpg").unwrap().dir);
        assert!(!e.iter().find(|x| x.name == "piste.mp3").unwrap().dir);
    }

    #[test]
    fn les_entrees_speciales_et_cachees_sont_ecartees() {
        // `.` et `..` feraient tourner l'arbre en rond ; les entrées cachées
        // sont écartées comme le fait déjà `scan::list_dir`, pour que les deux
        // arbres se ressemblent.
        let noms: Vec<String> = parse_ls(LS).unwrap().into_iter().map(|x| x.name).collect();
        assert!(!noms.iter().any(|n| n == "." || n == ".." || n == ".cache"), "{noms:?}");
    }

    #[test]
    fn le_pied_de_sortie_nest_pas_une_entree() {
        let noms: Vec<String> = parse_ls(LS).unwrap().into_iter().map(|x| x.name).collect();
        assert!(!noms.iter().any(|n| n.contains("blocks")), "{noms:?}");
    }

    #[test]
    fn un_dossier_vide_rend_une_liste_vide_sans_erreur() {
        // Un vrai dossier vide ne contient que `.` et `..` : il doit se
        // distinguer d'une sortie inanalysable.
        let vide = "  .    D    0  Mon Aug 11 20:12:33 2025\n  ..   D    0  Mon Aug 11 20:12:33 2025\n";
        assert_eq!(parse_ls(vide).unwrap(), vec![]);
        assert_eq!(parse_ls("").unwrap(), vec![]);
    }

    #[test]
    fn une_sortie_non_vide_mais_inanalysable_est_une_erreur_et_non_un_dossier_vide() {
        // La décision qui compte. Si une version future change de format, un
        // dossier plein s'afficherait vide et l'utilisateur conclurait que son
        // NAS a perdu sa musique. Mieux vaut un refus qui nomme le problème.
        let err = parse_ls("quelque chose d'inattendu\nsur deux lignes\n").unwrap_err();
        assert!(matches!(err, SmbError::UnreadableOutput(_)), "{err:?}");
    }

    #[test]
    fn un_mot_de_passe_refuse_se_reconnait() {
        // Mesuré sur samba 4.19.5 : ce message part sur **stdout**, pas sur
        // stderr. Classer sur stderr seul raterait le cas le plus fréquent.
        assert_eq!(classify("session setup failed: NT_STATUS_LOGON_FAILURE"), SmbError::BadCredentials);
        assert_eq!(classify("session setup failed: NT_STATUS_ACCESS_DENIED"), SmbError::AccessDenied);
    }

    #[test]
    fn un_hote_injoignable_se_reconnait() {
        // Sorties captées telles quelles sur samba 4.19.5.
        assert_eq!(
            classify("do_connect: Connection to 127.0.0.1 failed (Error NT_STATUS_CONNECTION_REFUSED)"),
            SmbError::Unreachable
        );
        assert_eq!(
            classify("do_connect: Connection to 192.0.2.1 failed (Error NT_STATUS_IO_TIMEOUT)"),
            SmbError::Unreachable
        );
    }

    #[test]
    fn un_dossier_absent_nest_pas_un_hote_injoignable() {
        // Le piège que la mesure a démasqué. Le NAS rend
        // NT_STATUS_OBJECT_NAME_NOT_FOUND pour un dossier qui n'existe pas ;
        // le ranger avec les échecs de connexion ferait lire « la machine n'a
        // pas répondu » devant un simple chemin périmé — et l'utilisateur
        // irait vérifier son réseau au lieu de son arborescence.
        assert_eq!(
            classify("cd \\NExistePas\\: NT_STATUS_OBJECT_NAME_NOT_FOUND"),
            SmbError::NotFound
        );
    }

    #[test]
    fn une_erreur_inconnue_part_verbatim() {
        // Inventer une phrase générique perdrait la seule information
        // disponible pour diagnostiquer.
        let e = classify("NT_STATUS_SOMETHING_NEW: le futur");
        assert_eq!(e, SmbError::Other("NT_STATUS_SOMETHING_NEW: le futur".into()));
    }

    #[test]
    fn un_echec_muet_reste_un_message_non_vide() {
        assert!(matches!(classify("   \n"), SmbError::Other(_)));
    }

    #[test]
    fn chaque_refus_resout_contre_le_catalogue_embarque() {
        // `Catalog::get` rend la clé quand il ne la trouve pas : sans ce test,
        // une faute de frappe afficherait « smb_bad_credentials » à l'écran.
        let c = catalogue();
        for e in [
            SmbError::NotInstalled,
            SmbError::BadCredentials,
            SmbError::AccessDenied,
            SmbError::Unreachable,
            SmbError::NotFound,
            SmbError::Timeout,
            SmbError::UnreadableOutput("brut".into()),
        ] {
            let m = e.message(&c, "nas");
            assert!(m.contains(' '), "cle brute renvoyee a l'ecran : {m:?}");
            assert!(!m.contains('{'), "jeton laisse tel quel : {m:?}");
        }
        assert!(SmbError::Unreachable.message(&c, "nas").contains("nas"));
    }
}
```

- [ ] **Step 3 : Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files smb"`
Expected: FAIL à la compilation.

- [ ] **Step 4 : Écrire l'implémentation**

Insérer au-dessus du bloc de tests :

```rust
//! Dialogue avec `smbclient` : énumérer les partages d'un hôte et lister un
//! dossier, **sans rien monter**.
//!
//! C'est ce qui permet de parcourir *avant* de déclarer. Monter
//! provisoirement pour prévisualiser aurait demandé un privilège pour un simple
//! coup d'œil, laissé des montages orphelins si l'onglet se ferme, et surtout
//! n'aurait pas su énumérer les partages — `mount.cifs` exige déjà de connaître
//! le nom du partage, ce qui est précisément la question qu'on pose à la
//! machine.
//!
//! L'analyse des sorties est pure et se teste sans NAS. Les formats sont ceux
//! de samba 4.19.5.

use ritornello_i18n::Catalog;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmbError {
    NotInstalled,
    BadCredentials,
    AccessDenied,
    Unreachable,
    /// Le partage ou le dossier visé n'existe pas.
    ///
    /// Distinct d'`Unreachable` **parce que la mesure l'a imposé** : le NAS
    /// rend `NT_STATUS_OBJECT_NAME_NOT_FOUND` dans ce cas, et les ranger
    /// ensemble ferait lire « la machine n'a pas répondu » devant un chemin
    /// périmé — l'utilisateur irait vérifier son réseau au lieu de son
    /// arborescence.
    NotFound,
    Timeout,
    /// Sortie non vide qu'aucune règle n'a su lire.
    ///
    /// Cas distinct d'un dossier vide **à dessein** : si une version de samba
    /// change de format, un dossier plein s'afficherait vide et l'utilisateur
    /// conclurait que son NAS a perdu sa musique. Un refus qui nomme le
    /// problème et rend la sortie brute est diagnosticable ; un dossier vide
    /// ne l'est pas.
    UnreadableOutput(String),
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SmbEntry {
    pub name: String,
    pub dir: bool,
}

impl SmbError {
    pub fn message(&self, catalog: &Catalog, host: &str) -> String {
        match self {
            SmbError::NotInstalled => catalog.get("smb_not_installed").to_string(),
            SmbError::BadCredentials => catalog.get("smb_bad_credentials").to_string(),
            SmbError::AccessDenied => catalog.get("smb_access_denied").to_string(),
            SmbError::Unreachable => catalog.get("smb_unreachable").replace("{host}", host),
            SmbError::NotFound => catalog.get("smb_not_found").to_string(),
            SmbError::Timeout => catalog.get("smb_timeout").replace("{host}", host),
            SmbError::UnreadableOutput(brut) => {
                catalog.get("smb_unreadable_output").replace("{detail}", brut)
            }
            // Verbatim : un code NT_STATUS inconnu est la seule information
            // disponible, et une phrase maison la perdrait.
            SmbError::Other(m) => m.clone(),
        }
    }
}

impl std::fmt::Display for SmbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmbError::NotInstalled => write!(f, "smbclient is not installed"),
            SmbError::BadCredentials => write!(f, "smb credentials refused"),
            SmbError::AccessDenied => write!(f, "smb access denied"),
            SmbError::Unreachable => write!(f, "smb host unreachable"),
            SmbError::NotFound => write!(f, "smb share or folder not found"),
            SmbError::Timeout => write!(f, "smb host timed out"),
            SmbError::UnreadableOutput(b) => write!(f, "unreadable smbclient output: {b}"),
            SmbError::Other(m) => write!(f, "smbclient: {m}"),
        }
    }
}

impl std::error::Error for SmbError {}

/// Classe les sorties de `smbclient`.
///
/// **Prend stdout et stderr réunis, et ce n'est pas un détail** : mesuré sur
/// samba 4.19.5, `do_connect: … failed (Error NT_STATUS_…)` part sur stderr,
/// mais `session setup failed: NT_STATUS_ACCESS_DENIED` part sur **stdout**.
/// Classer sur stderr seul raterait l'échec d'authentification, c'est-à-dire le
/// cas le plus fréquent chez l'utilisateur.
pub fn classify(sorties: &str) -> SmbError {
    let s = sorties.to_ascii_uppercase();
    if s.contains("NT_STATUS_LOGON_FAILURE")
        || s.contains("NT_STATUS_WRONG_PASSWORD")
        || s.contains("NT_STATUS_NO_SUCH_USER")
        || s.contains("NT_STATUS_ACCOUNT_DISABLED")
    {
        return SmbError::BadCredentials;
    }
    if s.contains("NT_STATUS_ACCESS_DENIED") {
        return SmbError::AccessDenied;
    }
    // `OBJECT_NAME_NOT_FOUND` se teste **avant** les échecs de connexion et
    // n'en fait surtout pas partie : le NAS le rend pour un dossier absent.
    if s.contains("NT_STATUS_OBJECT_NAME_NOT_FOUND")
        || s.contains("NT_STATUS_BAD_NETWORK_NAME")
        || s.contains("NT_STATUS_OBJECT_PATH_NOT_FOUND")
    {
        return SmbError::NotFound;
    }
    if s.contains("NT_STATUS_CONNECTION_REFUSED")
        || s.contains("NT_STATUS_IO_TIMEOUT")
        || s.contains("NT_STATUS_HOST_UNREACHABLE")
        || s.contains("NT_STATUS_NETWORK_UNREACHABLE")
        || s.contains("FAILED TO CONNECT")
    {
        return SmbError::Unreachable;
    }
    let t = sorties.trim();
    if t.is_empty() {
        SmbError::Other("smbclient failed without a message".to_string())
    } else {
        SmbError::Other(t.to_string())
    }
}

/// Analyse la sortie de `smbclient -L //hôte -g`.
///
/// Le format machine (`Type|nom|commentaire`) plutôt que le tableau humain :
/// celui-ci change de largeur de colonne selon les versions, et l'analyser
/// aurait été un défaut qui n'apparaît que sur la machine de quelqu'un d'autre.
///
/// Les partages administratifs (`IPC$`, `print$`, tout nom finissant par `$`)
/// sont écartés : ils ne contiennent pas de musique et leur présence ferait
/// douter du bon partage.
pub fn parse_shares(stdout: &str) -> Vec<String> {
    let mut out: Vec<String> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("Disk|"))
        .map(|reste| reste.split('|').next().unwrap_or("").trim().to_string())
        .filter(|n| !n.is_empty() && !n.ends_with('$'))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Analyse la sortie de `smbclient -c 'ls'`.
///
/// Le format est positionnel et se lit **par la droite** : la date occupe les
/// cinq derniers mots, la taille le sixième, les attributs le septième ; le nom
/// est tout ce qui reste, y compris ses espaces. Lire par la gauche casserait
/// sur le premier nom d'album contenant un espace, c'est-à-dire presque tous.
///
/// Une ligne qu'aucune règle ne lit est comptée : si la sortie n'était pas vide
/// et que rien n'a été reconnu, c'est un `UnreadableOutput` et non un dossier
/// vide (voir la variante).
pub fn parse_ls(stdout: &str) -> Result<Vec<SmbEntry>, SmbError> {
    let mut entrees = Vec::new();
    let mut lues = 0usize;
    let mut ignorees = Vec::new();

    for ligne in stdout.lines() {
        if ligne.trim().is_empty() {
            continue;
        }
        // Le pied de sortie : « 9876543 blocks of size 1024. … ».
        if ligne.contains("blocks of size") {
            lues += 1;
            continue;
        }
        match decoupe(ligne) {
            Some((nom, attrs)) => {
                lues += 1;
                // `.` et `..` feraient tourner l'arbre en rond ; les entrées
                // cachées sont écartées comme le fait déjà `scan::list_dir`,
                // pour que les deux arbres se ressemblent.
                if nom == "." || nom == ".." || nom.starts_with('.') {
                    continue;
                }
                entrees.push(SmbEntry { name: nom.to_string(), dir: attrs.contains('D') });
            }
            None => ignorees.push(ligne.trim()),
        }
    }

    if lues == 0 && !ignorees.is_empty() {
        return Err(SmbError::UnreadableOutput(ignorees.join(" / ")));
    }
    entrees.sort_by(|a, b| (b.dir, &a.name).cmp(&(a.dir, &b.name)));
    Ok(entrees)
}

/// Découpe une ligne de `ls` par la droite : rend `(nom, attributs)`.
///
/// Les attributs peuvent être **absents** sur certaines versions ; dans ce cas
/// la colonne repérée n'est pas faite que de lettres d'attribut, et on la rend
/// au nom plutôt que de tronquer celui-ci.
fn decoupe(ligne: &str) -> Option<(&str, &str)> {
    const ATTRS: &str = "DAHNRSE";
    let mut reste = ligne.trim_end();
    // Cinq mots de date : « Mon Aug 11 20:12:33 2025 ».
    for _ in 0..5 {
        reste = reste[..reste.rfind(char::is_whitespace)?].trim_end();
    }
    // La taille, qui doit être un nombre — c'est ce qui distingue une vraie
    // ligne d'entrée d'une phrase de diagnostic à cinq mots ou plus.
    let avant_taille = reste[..reste.rfind(char::is_whitespace)?].trim_end();
    let taille = reste[avant_taille.len()..].trim();
    if taille.is_empty() || !taille.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // Les attributs, s'ils forment bien une colonne d'attributs.
    let avant_attrs = match avant_taille.rfind(char::is_whitespace) {
        Some(i) => avant_taille[..i].trim_end(),
        None => return Some((avant_taille.trim(), "")),
    };
    let attrs = avant_taille[avant_attrs.len()..].trim();
    if !attrs.is_empty() && attrs.chars().all(|c| ATTRS.contains(c)) {
        let nom = avant_attrs.trim();
        (!nom.is_empty()).then_some((nom, attrs))
    } else {
        // Pas d'attributs : tout ce qui précède la taille est le nom.
        let nom = avant_taille.trim();
        (!nom.is_empty()).then_some((nom, ""))
    }
}
```

- [ ] **Step 5 : Lancer les tests**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files smb"`
Expected: PASS, 13 tests.

- [ ] **Step 6 : Commit**

```bash
git add crates/ritornello-plugin-files/src/smb.rs crates/ritornello-plugin-files/src/lib.rs
git commit -m "feat(plugin-files): lire ce que smbclient repond, et refuser de deviner"
```

---

## Task 5 : `smb.rs` — les appels, la sonde et le fichier d'authentification

**Files:**
- Modify: `crates/ritornello-plugin-files/src/smb.rs`

**Interfaces:**
- Consumes: `crate::roots::champ_sur` (T3), `SmbError`, `parse_shares`, `parse_ls`, `classify` (T4).
- Produces:
  - `pub struct Credentials { pub user: String, pub password: String, pub domain: String }`
  - `pub async fn available() -> bool`
  - `pub async fn list_shares(host: &str, creds: Option<&Credentials>, dir_travail: &Path, delai: Duration) -> Result<Vec<String>, SmbError>`
  - `pub async fn list_dir(host: &str, share: &str, path: &str, creds: Option<&Credentials>, dir_travail: &Path, delai: Duration) -> Result<Vec<SmbEntry>, SmbError>`

- [ ] **Step 1 : Écrire les tests qui échouent**

Ajouter dans le `mod tests` de `src/smb.rs` :

```rust
#[test]
fn un_argument_qui_ressemble_a_une_option_est_refuse() {
    // `smbclient` lirait « -L » comme un drapeau. Un hôte nommé « -L » n'a
    // aucun sens, mais il vient du navigateur : la ligne de commande ne doit
    // pas pouvoir être réécrite depuis le formulaire.
    assert!(!argument_sur("-L"));
    assert!(!argument_sur("--user=root"));
    assert!(!argument_sur(""));
    assert!(argument_sur("192.168.1.20"));
    assert!(argument_sur("nas.local"));
}

#[cfg(unix)]
#[tokio::test]
async fn le_fichier_d_authentification_est_en_0600_et_disparait() {
    // Le mot de passe ne passe jamais par argv — il y serait lisible dans `ps`
    // par tout utilisateur de la machine. Les permissions sont posées à la
    // création, pas après : créer puis restreindre laisse une fenêtre.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let creds = Credentials {
        user: "steven".into(),
        password: "secret-du-nas".into(),
        domain: String::new(),
    };
    let chemin = {
        let f = FichierAuth::creer(dir.path(), &creds).unwrap();
        let meta = std::fs::metadata(f.chemin()).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let contenu = std::fs::read_to_string(f.chemin()).unwrap();
        assert!(contenu.contains("password=secret-du-nas"), "{contenu}");
        f.chemin().to_path_buf()
    };
    // Le fichier s'efface à la libération : un mot de passe ne doit pas
    // survivre à l'appel qui l'a demandé.
    assert!(!chemin.exists(), "le fichier d'authentification a survecu");
}

#[tokio::test]
async fn un_hote_refuse_ne_lance_aucun_processus() {
    let dir = tempfile::tempdir().unwrap();
    let e = list_shares("-L", None, dir.path(), std::time::Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(matches!(e, SmbError::Other(_)), "{e:?}");
}
```

- [ ] **Step 2 : Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files smb"`
Expected: FAIL — `cannot find function 'argument_sur'`.

- [ ] **Step 3 : Écrire l'implémentation**

Ajouter en tête des `use` de `src/smb.rs` :

```rust
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
```

Puis, avant le `mod tests` :

```rust
/// Nom du binaire. Constante nommée pour que le parcours de bout en bout puisse
/// poser un faux `smbclient` dans le `PATH` du serveur d'essai.
const SMBCLIENT: &str = "smbclient";

pub struct Credentials {
    pub user: String,
    pub password: String,
    pub domain: String,
}

/// Vrai si la valeur peut être posée telle quelle dans une ligne de commande.
///
/// Un argument commençant par `-` serait lu par `smbclient` comme un drapeau :
/// le formulaire pourrait alors réécrire la ligne de commande. `champ_sur`
/// couvre déjà la virgule, l'espace, `..` et l'octet nul.
fn argument_sur(v: &str) -> bool {
    crate::roots::champ_sur(v) && !v.starts_with('-')
}

/// Fichier d'authentification temporaire, effacé à la libération.
///
/// Le mot de passe ne passe **jamais** par `argv` : il y serait lisible dans
/// `ps` par tout utilisateur de la machine. Les permissions sont posées **à la
/// création** — créer puis restreindre laisserait une fenêtre pendant laquelle
/// le secret serait lisible par tout le monde.
struct FichierAuth(PathBuf);

impl FichierAuth {
    fn creer(dir: &Path, creds: &Credentials) -> std::io::Result<Self> {
        static COMPTEUR: AtomicU64 = AtomicU64::new(0);
        std::fs::create_dir_all(dir)?;
        let n = COMPTEUR.fetch_add(1, Ordering::Relaxed);
        let chemin = dir.join(format!(".explore-{}-{n}.auth", std::process::id()));
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&chemin)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&chemin)?;
        use std::io::Write;
        writeln!(f, "username={}", creds.user)?;
        writeln!(f, "password={}", creds.password)?;
        if !creds.domain.is_empty() {
            writeln!(f, "domain={}", creds.domain)?;
        }
        f.sync_all()?;
        Ok(Self(chemin))
    }

    fn chemin(&self) -> &Path {
        &self.0
    }
}

impl Drop for FichierAuth {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Vrai si `smbclient` est présent et exécutable.
///
/// Sonde plutôt que présomption : son absence doit griser l'assistant, pas
/// faire échouer une action au pire moment (voir `can_browse_smb`).
pub async fn available() -> bool {
    tokio::process::Command::new(SMBCLIENT)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Lance `smbclient` et rend `(stdout, sorties réunies, succès)`.
///
/// Le délai est tenu ici, par `tokio`, et non par le `-t` de `smbclient` : sa
/// présence et sa sémantique varient selon les versions, là où tuer le
/// processus est vrai partout. Sans ce plafond, un NAS éteint retiendrait la
/// tâche bien au-delà de ce que la page attend.
async fn lancer(args: &[String], delai: Duration) -> Result<(String, String, bool), SmbError> {
    let mut enfant = tokio::process::Command::new(SMBCLIENT)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SmbError::NotInstalled
            } else {
                SmbError::Other(e.to_string())
            }
        })?;
    let sortie = match tokio::time::timeout(delai, enfant.wait_with_output()).await {
        Ok(r) => r.map_err(|e| SmbError::Other(e.to_string()))?,
        Err(_) => return Err(SmbError::Timeout),
    };
    let out = String::from_utf8_lossy(&sortie.stdout).to_string();
    let err = String::from_utf8_lossy(&sortie.stderr).to_string();
    // Les deux flux réunis pour le classement : mesuré sur samba 4.19.5, le
    // refus d'authentification part sur stdout et le refus de connexion sur
    // stderr.
    let reunies = format!("{out}\n{err}");
    Ok((out, reunies, sortie.status.success()))
}

/// Arguments d'authentification : fichier, ou tentative invité.
///
/// Utilisateur vide → `-N`. Beaucoup de NAS domestiques exposent un partage
/// public, et exiger un compte les rendrait inaccessibles.
fn args_auth(auth: &Option<FichierAuth>) -> Vec<String> {
    match auth {
        Some(f) => vec!["-A".to_string(), f.chemin().display().to_string()],
        None => vec!["-N".to_string()],
    }
}

fn prepare_auth(creds: Option<&Credentials>, dir: &Path) -> Result<Option<FichierAuth>, SmbError> {
    match creds {
        Some(c) if !c.user.is_empty() => {
            Some(FichierAuth::creer(dir, c).map_err(|e| SmbError::Other(e.to_string()))).transpose()
        }
        _ => Ok(None),
    }
}

/// Énumère les partages d'un hôte.
pub async fn list_shares(
    host: &str,
    creds: Option<&Credentials>,
    dir_travail: &Path,
    delai: Duration,
) -> Result<Vec<String>, SmbError> {
    if !argument_sur(host) {
        return Err(SmbError::Other(format!("invalid host: {host}")));
    }
    let auth = prepare_auth(creds, dir_travail)?;
    let mut args = vec!["-L".to_string(), format!("//{host}"), "-g".to_string()];
    args.extend(args_auth(&auth));
    let (out, reunies, ok) = lancer(&args, delai).await?;
    if !ok {
        return Err(classify(&reunies));
    }
    Ok(parse_shares(&out))
}

/// Liste un dossier d'un partage.
///
/// Le répertoire de départ passe par `-D` plutôt que par un `cd "…"` glissé
/// dans la chaîne `-c` : un nom contenant un guillemet casserait l'analyse que
/// `smbclient` fait de sa propre commande.
pub async fn list_dir(
    host: &str,
    share: &str,
    path: &str,
    creds: Option<&Credentials>,
    dir_travail: &Path,
    delai: Duration,
) -> Result<Vec<SmbEntry>, SmbError> {
    if !argument_sur(host) {
        return Err(SmbError::Other(format!("invalid host: {host}")));
    }
    if !argument_sur(share) {
        return Err(SmbError::Other(format!("invalid share: {share}")));
    }
    let depart = if path.is_empty() { "/".to_string() } else { format!("/{path}") };
    if depart.starts_with("-") || depart.contains('\0') || depart.contains("..") {
        return Err(SmbError::Other(format!("invalid path: {path}")));
    }
    let auth = prepare_auth(creds, dir_travail)?;
    let mut args =
        vec![format!("//{host}/{share}"), "-D".to_string(), depart, "-c".to_string(), "ls".to_string()];
    args.extend(args_auth(&auth));
    let (out, reunies, ok) = lancer(&args, delai).await?;
    if !ok {
        return Err(classify(&reunies));
    }
    parse_ls(&out)
}
```

- [ ] **Step 4 : Lancer les tests**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files smb"`
Expected: PASS, 16 tests.

- [ ] **Step 5 : Commit**

```bash
git add crates/ritornello-plugin-files/src/smb.rs
git commit -m "feat(plugin-files): appeler smbclient sans jamais poser le mot de passe dans argv"
```

---

## Task 6 : `explore.rs` — l'état des deux assistants

**Files:**
- Create: `crates/ritornello-plugin-files/src/explore.rs`
- Modify: `crates/ritornello-plugin-files/src/lib.rs`

**Interfaces:**
- Consumes: `crate::volumes` (T2), `crate::smb` (T4, T5), `crate::scan::list_dir`, `Catalog`.
- Produces:
  - `pub enum Kind { Local, Smb }` (Deserialize, `rename_all = "snake_case"`)
  - `pub struct Explorateur` avec `new(creds_dir, catalog, smb_ok)`
  - `pub fn ouvrir(&mut self, kind: Kind)`, `pub fn fermer(&mut self)`
  - `pub fn local(&mut self, path: &str) -> Result<(), String>`
  - `pub fn connecter(&mut self, host: String, user: String, password: String, domain: String)`
  - `pub fn parcourir(&mut self, share: String, path: String)`
  - `pub fn credentials(&self, host: &str) -> Option<smb::Credentials>`
  - `pub fn vue(&self) -> serde_json::Value`

- [ ] **Step 1 : Déclarer le module**

Dans `src/lib.rs`, ajouter `pub mod explore;`.

- [ ] **Step 2 : Écrire les tests qui échouent**

Créer `src/explore.rs` avec seulement ce bloc de tests :

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn explorateur(dir: &std::path::Path) -> Explorateur {
        Explorateur::new(
            dir.join("creds"),
            Arc::new(std::sync::RwLock::new(Catalog::load(
                "files",
                "en",
                std::path::Path::new("/inexistant"),
                crate::FILES_EN,
            ))),
            Arc::new(AtomicBool::new(true)),
        )
    }

    #[tokio::test]
    async fn le_mot_de_passe_n_apparait_dans_aucune_vue() {
        // Il n'a aucune raison de retraverser vers le navigateur : la page l'a
        // envoyé une fois, elle n'a pas besoin de le relire pour afficher un
        // arbre de dossiers.
        let dir = tempfile::tempdir().unwrap();
        let mut e = explorateur(dir.path());
        e.ouvrir(Kind::Smb);
        e.connecter("nas".into(), "steven".into(), "secret-du-nas".into(), String::new());
        let texte = serde_json::to_string(&e.vue()).unwrap();
        assert!(!texte.contains("secret-du-nas"), "{texte}");
        assert!(!texte.contains("password"), "{texte}");
    }

    #[tokio::test]
    async fn fermer_efface_la_session() {
        // Sinon un mot de passe survivrait en mémoire à la popin qui l'a
        // recueilli, sans que rien ne le reprenne jamais.
        let dir = tempfile::tempdir().unwrap();
        let mut e = explorateur(dir.path());
        e.ouvrir(Kind::Smb);
        e.connecter("nas".into(), "steven".into(), "secret".into(), String::new());
        assert!(e.credentials("nas").is_some());
        e.fermer();
        assert!(e.credentials("nas").is_none());
    }

    #[tokio::test]
    async fn un_chemin_local_hors_volume_est_refuse() {
        // La garde de parcours. Sans elle, la page adresserait /proc/self et
        // l'arbre partirait dans les liens récursifs.
        let dir = tempfile::tempdir().unwrap();
        let faux = dir.path().join("mounts");
        std::fs::write(&faux, "proc /proc proc rw 0 0\n/dev/sda1 / ext4 rw 0 0\n").unwrap();
        std::env::set_var("RITORNELLO_FILES_PROC_MOUNTS", &faux);
        let mut e = explorateur(dir.path());
        e.ouvrir(Kind::Local);
        let err = e.local("/proc/self").unwrap_err();
        assert!(err.contains(' '), "cle brute : {err}");
        std::env::remove_var("RITORNELLO_FILES_PROC_MOUNTS");
    }

    #[tokio::test]
    async fn un_dossier_local_rend_ses_sous_dossiers_et_son_compte_audio() {
        // Le compte de fichiers audio est ce qui dit qu'on est au bon endroit :
        // sans lui on choisit un dossier en espérant.
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media");
        std::fs::create_dir_all(media.join("Album")).unwrap();
        std::fs::write(media.join("a.mp3"), b"").unwrap();
        std::fs::write(media.join("b.flac"), b"").unwrap();
        std::fs::write(media.join("notes.txt"), b"").unwrap();
        let faux = dir.path().join("mounts");
        std::fs::write(&faux, format!("/dev/sda1 {} ext4 rw 0 0\n", dir.path().display())).unwrap();
        std::env::set_var("RITORNELLO_FILES_PROC_MOUNTS", &faux);
        let mut e = explorateur(dir.path());
        e.ouvrir(Kind::Local);
        e.local(&media.display().to_string()).unwrap();
        let v = e.vue();
        assert_eq!(v["dirs"], serde_json::json!(["Album"]));
        assert_eq!(v["audio_count"], 2, "notes.txt n'est pas un fichier audio");
        std::env::remove_var("RITORNELLO_FILES_PROC_MOUNTS");
    }
}
```

- [ ] **Step 3 : Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files explore"`
Expected: FAIL à la compilation.

- [ ] **Step 4 : Écrire l'implémentation**

Insérer au-dessus du bloc de tests :

```rust
//! L'état des deux assistants de déclaration de source.
//!
//! Extrait de `admin.rs`, qui atteignait 800 lignes : les opérations
//! d'assistant y auraient formé un deuxième sujet sans rapport avec la gestion
//! de la liste de lecture.
//!
//! Le protocole admin étant requête/réponse et ne poussant rien, une connexion
//! réseau ne peut pas être attendue dans la requête : un NAS éteint dépasserait
//! le plafond de 5 s du cœur et la requête serait tuée avant d'avoir rien
//! rapporté. `connecter` et `parcourir` lancent donc une tâche et rendent la
//! main aussitôt ; la page suit l'avancement par sondage, exactement comme pour
//! le balayage.

use crate::smb::{self, Credentials};
use crate::{scan, volumes};
use ritornello_i18n::Catalog;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// Plafond d'un appel `smbclient`. Large — un NAS qui se réveille prend son
/// temps — mais fini : la page doit toujours finir par apprendre quelque chose.
const DELAI_SMB: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Local,
    Smb,
}

/// Ce que la page lit de l'assistant en cours.
///
/// **Ne contient aucun identifiant.** La garantie est portée par le type, comme
/// pour `Root` : la structure sérialisée n'a pas de champ mot de passe, il n'y
/// a donc rien à filtrer et rien à oublier de filtrer.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Vue {
    pub open: bool,
    pub kind: Option<String>,
    pub host: String,
    pub share: String,
    pub path: String,
    pub shares: Vec<String>,
    pub dirs: Vec<String>,
    pub audio_count: usize,
    pub busy: bool,
    pub error: Option<String>,
}

pub struct Explorateur {
    creds_dir: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    smb_ok: Arc<AtomicBool>,
    vue: Arc<Mutex<Vue>>,
    /// Identifiants de la popin en cours, indexés par hôte.
    ///
    /// En mémoire et **jamais sérialisés** : le mot de passe traverse le fil
    /// une fois, à la connexion, et non à chaque clic dans l'arborescence.
    sessions: Arc<Mutex<HashMap<String, Credentials>>>,
    tache: Option<tokio::task::JoinHandle<()>>,
}

impl Explorateur {
    pub fn new(
        creds_dir: PathBuf,
        catalog: Arc<RwLock<Catalog>>,
        smb_ok: Arc<AtomicBool>,
    ) -> Self {
        Self {
            creds_dir,
            catalog,
            smb_ok,
            vue: Arc::new(Mutex::new(Vue::default())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            tache: None,
        }
    }

    fn mot(&self, cle: &str) -> String {
        self.catalog.read().unwrap().get(cle).to_string()
    }

    pub fn ouvrir(&mut self, kind: Kind) {
        self.annuler();
        *self.vue.lock().unwrap() = Vue {
            open: true,
            kind: Some(match kind {
                Kind::Local => "local".to_string(),
                Kind::Smb => "smb".to_string(),
            }),
            ..Vue::default()
        };
    }

    pub fn fermer(&mut self) {
        self.annuler();
        // Les identifiants meurent avec la popin : les laisser en mémoire
        // ferait survivre un mot de passe à ce qui l'a recueilli, sans que rien
        // ne le reprenne jamais.
        self.sessions.lock().unwrap().clear();
        *self.vue.lock().unwrap() = Vue::default();
    }

    fn annuler(&mut self) {
        if let Some(t) = self.tache.take() {
            t.abort();
        }
    }

    pub fn credentials(&self, host: &str) -> Option<Credentials> {
        self.sessions.lock().unwrap().get(host).map(|c| Credentials {
            user: c.user.clone(),
            password: c.password.clone(),
            domain: c.domain.clone(),
        })
    }

    /// Contenu d'un dossier de l'appareil.
    ///
    /// Synchrone : un système de fichiers local répond bien en deçà du plafond
    /// du cœur, et rendre cela asynchrone n'ajouterait qu'un aller-retour de
    /// sondage entre chaque niveau ouvert.
    pub fn local(&mut self, path: &str) -> Result<(), String> {
        let chemin = std::path::Path::new(path);
        let mounts = volumes::lire_proc_mounts();
        let canon = chemin
            .canonicalize()
            .map_err(|_| self.mot("bad_local_path").replace("{path}", path))?;
        if !volumes::parcourable(&mounts, &canon) {
            return Err(self.mot("bad_local_path").replace("{path}", path));
        }
        let (dossiers, fichiers) = scan::list_dir(&canon)
            .map_err(|e| e.message(&self.catalog.read().unwrap()))?;
        let mut v = self.vue.lock().unwrap();
        v.path = canon.display().to_string();
        v.dirs = dossiers;
        v.audio_count = fichiers.len();
        v.error = None;
        v.busy = false;
        Ok(())
    }

    /// Se connecte à un hôte et énumère ses partages.
    pub fn connecter(&mut self, host: String, user: String, password: String, domain: String) {
        self.annuler();
        if !user.is_empty() {
            self.sessions
                .lock()
                .unwrap()
                .insert(host.clone(), Credentials { user, password, domain });
        }
        if !self.smb_ok.load(Ordering::Relaxed) {
            self.echec(smb::SmbError::NotInstalled, &host);
            return;
        }
        {
            let mut v = self.vue.lock().unwrap();
            v.host = host.clone();
            v.share = String::new();
            v.path = String::new();
            v.shares.clear();
            v.dirs.clear();
            v.busy = true;
            v.error = None;
        }
        let creds = self.credentials(&host);
        let dir = self.creds_dir.clone();
        let vue = self.vue.clone();
        let catalog = self.catalog.clone();
        self.tache = Some(tokio::spawn(async move {
            let r = smb::list_shares(&host, creds.as_ref(), &dir, DELAI_SMB).await;
            let mut v = vue.lock().unwrap();
            v.busy = false;
            match r {
                Ok(partages) => {
                    v.shares = partages;
                    v.error = None;
                }
                Err(e) => {
                    tracing::warn!("listing shares of {host}: {e}");
                    v.error = Some(e.message(&catalog.read().unwrap(), &host));
                }
            }
        }));
    }

    /// Liste un dossier d'un partage.
    pub fn parcourir(&mut self, share: String, path: String) {
        self.annuler();
        let host = self.vue.lock().unwrap().host.clone();
        if !self.smb_ok.load(Ordering::Relaxed) {
            self.echec(smb::SmbError::NotInstalled, &host);
            return;
        }
        {
            let mut v = self.vue.lock().unwrap();
            v.share = share.clone();
            v.path = path.clone();
            v.dirs.clear();
            v.audio_count = 0;
            v.busy = true;
            v.error = None;
        }
        let creds = self.credentials(&host);
        let dir = self.creds_dir.clone();
        let vue = self.vue.clone();
        let catalog = self.catalog.clone();
        self.tache = Some(tokio::spawn(async move {
            let r = smb::list_dir(&host, &share, &path, creds.as_ref(), &dir, DELAI_SMB).await;
            let mut v = vue.lock().unwrap();
            v.busy = false;
            match r {
                Ok(entrees) => {
                    v.dirs = entrees.iter().filter(|e| e.dir).map(|e| e.name.clone()).collect();
                    v.audio_count = entrees
                        .iter()
                        .filter(|e| !e.dir && scan::is_audio(std::path::Path::new(&e.name)))
                        .count();
                    v.error = None;
                }
                Err(e) => {
                    tracing::warn!("listing //{host}/{share}/{path}: {e}");
                    v.error = Some(e.message(&catalog.read().unwrap(), &host));
                }
            }
        }));
    }

    fn echec(&self, e: smb::SmbError, host: &str) {
        let mut v = self.vue.lock().unwrap();
        v.busy = false;
        v.error = Some(e.message(&self.catalog.read().unwrap(), host));
    }

    pub fn vue(&self) -> serde_json::Value {
        serde_json::to_value(&*self.vue.lock().unwrap()).unwrap_or_default()
    }
}
```

- [ ] **Step 5 : Lancer les tests**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files explore -- --test-threads=1"`
Expected: PASS, 4 tests. `--test-threads=1` parce que deux tests manipulent la même variable d'environnement.

- [ ] **Step 6 : Commit**

```bash
git add crates/ritornello-plugin-files/src/explore.rs crates/ritornello-plugin-files/src/lib.rs
git commit -m "feat(plugin-files): l etat des assistants, et les identifiants qui meurent avec la popin"
```

---

## Task 7 : `admin.rs` — les opérations de source

**Files:**
- Modify: `crates/ritornello-plugin-files/src/admin.rs`

**Interfaces:**
- Consumes: `Explorateur` (T6), `volumes` (T2), `derive_name` (T3).
- Produces: le contrat de `get_data` décrit ci-dessous, consommé par T9.

- [ ] **Step 1 : Écrire les tests qui échouent**

Ajouter dans le `mod tests` de `src/admin.rs` :

```rust
fn ajout_partage(password: &str) -> serde_json::Value {
    serde_json::json!({
        "op": "add_source", "kind": "smb", "host": "192.168.1.20",
        "share": "musique", "subpath": "Ma Musique", "user": "steven",
        "domain": "", "writable": false, "password": password
    })
}

#[tokio::test]
async fn une_source_ajoutee_recoit_un_nom_derive() {
    // L'utilisateur ne saisit plus de nom : il doit être dérivé, valide, et
    // dérivé du partage pour rester lisible dans /mnt/ritornello.
    let (mut admin, _) = admin_de_test();
    admin.set_data(ajout_partage("p")).await.unwrap();
    let roots = admin.roots.read().await;
    assert_eq!(roots.root.len(), 1);
    assert_eq!(roots.root[0].name, "musique");
    assert_eq!(roots.root[0].subpath.as_deref(), Some("Ma Musique"));
}

#[tokio::test]
async fn deux_sources_du_meme_partage_ne_se_disputent_pas_leur_nom() {
    // Sans dédoublonnage, la deuxième écraserait le fichier d'identifiants de
    // la première et se disputerait son point de montage.
    let (mut admin, _) = admin_de_test();
    admin.set_data(ajout_partage("p")).await.unwrap();
    let mut second = ajout_partage("p");
    second["subpath"] = serde_json::json!("Rock");
    admin.set_data(second).await.unwrap();
    let roots = admin.roots.read().await;
    assert_eq!(roots.root.len(), 2);
    assert_ne!(roots.root[0].name, roots.root[1].name);
}

#[tokio::test]
async fn le_doublon_exact_est_refuse() {
    // Deux sources identiques monteraient deux fois le même partage au même
    // endroit logique, sans qu'aucune ne serve à rien de plus.
    let (mut admin, _) = admin_de_test();
    admin.set_data(ajout_partage("p")).await.unwrap();
    let err = admin.set_data(ajout_partage("p")).await.unwrap_err();
    assert!(err.contains(' '), "cle brute : {err}");
}

#[tokio::test]
async fn retirer_une_source_efface_son_fichier_d_identifiants() {
    // Sinon un .cred contenant un mot de passe survivrait sur le disque à la
    // source qui l'a justifié.
    let (mut admin, _) = admin_de_test();
    admin.set_data(ajout_partage("secret")).await.unwrap();
    let cred = admin.creds_dir.join("musique.cred");
    assert!(cred.exists());
    admin
        .set_data(serde_json::json!({"op": "remove_source", "name": "musique"}))
        .await
        .unwrap();
    assert!(!cred.exists(), "le fichier d'identifiants a survecu a la source");
    assert!(admin.roots.read().await.root.is_empty());
}

#[tokio::test]
async fn get_data_annonce_les_volumes_et_la_capacite_smb() {
    let (admin, racine) = admin_de_test();
    let faux = racine.join("mounts");
    std::fs::write(&faux, "/dev/sda1 /media/usb vfat rw 0 0\nproc /proc proc rw 0 0\n").unwrap();
    std::env::set_var("RITORNELLO_FILES_PROC_MOUNTS", &faux);
    let d = admin.get_data().await;
    assert_eq!(d["volumes"][0]["path"], "/media/usb");
    assert_eq!(d["volumes"].as_array().unwrap().len(), 1, "proc ne doit pas etre propose");
    assert!(d["can_browse_smb"].is_boolean());
    assert!(d["explore"].is_object());
    std::env::remove_var("RITORNELLO_FILES_PROC_MOUNTS");
}

#[tokio::test]
async fn basculer_l_inscriptibilite_ne_perd_pas_le_mot_de_passe() {
    // Sans cette opération, changer d'avis imposerait de retirer puis
    // redéclarer, donc de resaisir le mot de passe.
    let (mut admin, _) = admin_de_test();
    admin.set_data(ajout_partage("secret-du-nas")).await.unwrap();
    admin
        .set_data(serde_json::json!({"op": "set_writable", "name": "musique", "writable": true}))
        .await
        .unwrap();
    assert!(admin.roots.read().await.by_name("musique").unwrap().writable);
    let cred = std::fs::read_to_string(admin.creds_dir.join("musique.cred")).unwrap();
    assert!(cred.contains("password=secret-du-nas"), "{cred}");
}
```

Supprimer les tests devenus caducs : `une_racine_invalide_est_refusee_par_une_phrase_qui_nomme_le_fautif`, `une_racine_refusee_ne_laisse_aucun_fichier_d_identifiants`, `la_table_enregistree_se_relit_telle_quelle`, `get_data_ne_rend_jamais_le_mot_de_passe`, `un_mot_de_passe_vide_conserve_celui_deja_enregistre`, `un_mot_de_passe_neuf_remplace_l_ancien`, `le_fichier_d_identifiants_est_ecrit_en_0600` — **puis les réécrire** contre `add_source` en remplaçant `partage(p)` par `ajout_partage(p)` et `"op": "save_roots"` par la forme ci-dessus. Leur intention reste valable ; seule l'opération change.

- [ ] **Step 2 : Lancer les tests pour vérifier qu'ils échouent**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files admin"`
Expected: FAIL — `unknown variant 'add_source'`.

- [ ] **Step 3 : Remplacer l'énumération des opérations**

Dans `src/admin.rs`, remplacer `Op::SaveRoots` par :

```rust
    AddSource {
        kind: RootKind,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        host: String,
        #[serde(default)]
        share: String,
        #[serde(default)]
        subpath: Option<String>,
        #[serde(default)]
        user: String,
        #[serde(default)]
        domain: String,
        /// **Vide veut dire « prends celui de la session, à défaut celui déjà
        /// enregistré »**. La page ne peut pas renvoyer un secret qu'elle ne
        /// reçoit jamais, et l'assistant ne doit pas le faire retaper à la
        /// confirmation alors qu'il vient de servir à se connecter.
        #[serde(default)]
        password: String,
        #[serde(default)]
        writable: bool,
    },
    RemoveSource { name: String },
    SetWritable { name: String, writable: bool },
    ExploreOpen { kind: ritornello_plugin_files::explore::Kind },
    ExploreClose,
    ExploreLocal { path: String },
    SmbConnect {
        host: String,
        #[serde(default)]
        user: String,
        #[serde(default)]
        password: String,
        #[serde(default)]
        domain: String,
    },
    SmbBrowse {
        share: String,
        #[serde(default)]
        path: String,
    },
```

Supprimer la variante `SaveRoots` et son bras dans `set_data`. Garder `Mount`.

- [ ] **Step 4 : Ajouter les champs de structure**

Dans `pub struct FilesAdmin`, ajouter :

```rust
    /// L'assistant en cours. Vit ici plutôt que dans son propre verrou : une
    /// seule popin est ouverte à la fois, et le protocole admin est
    /// séquentiel.
    pub explore: ritornello_plugin_files::explore::Explorateur,
    /// Résultat de la dernière réconciliation de montage.
    ///
    /// Le montage suit désormais la déclaration : l'utilisateur ne clique plus
    /// « Monter ». Un échec ne doit donc pas se perdre — sans ce champ, une
    /// source déclarée resterait « non montée » sans jamais dire pourquoi.
    pub mount_error: Arc<Mutex<Option<String>>>,
    /// `smbclient` est-il utilisable. Sondé au démarrage, resondé à chaque
    /// tentative de connexion.
    pub smb_ok: Arc<std::sync::atomic::AtomicBool>,
```

- [ ] **Step 5 : Écrire les bras de `set_data`**

Ajouter dans le `match op` :

```rust
            Op::AddSource {
                kind, path, host, share, subpath, user, domain, password, writable,
            } => {
                let mut table = self.roots.read().await.clone();
                // Le doublon exact seul est refusé : deux dossiers différents
                // du même partage sont deux sources légitimes, qui montent le
                // partage deux fois — ce qui est légal, peu coûteux, et surtout
                // sans surprise. Fusionner en élargissant le sous-chemin commun
                // modifierait en silence la portée d'une source déjà déclarée.
                let deja = table.root.iter().any(|r| {
                    r.kind == kind
                        && r.host == host
                        && r.share == share
                        && r.subpath == subpath
                        && r.path == path
                });
                if deja {
                    return Err(self.mot("duplicate_source"));
                }
                let pris: Vec<&str> = table.root.iter().map(|r| r.name.as_str()).collect();
                let indice = match kind {
                    RootKind::Smb => share.clone(),
                    RootKind::Local => path
                        .clone()
                        .unwrap_or_default()
                        .rsplit('/')
                        .find(|s| !s.is_empty())
                        .unwrap_or("disque")
                        .to_string(),
                };
                let name = ritornello_plugin_files::roots::derive_name(&indice, &pris);
                let racine = Root {
                    name: name.clone(), kind, path, host: host.clone(), share,
                    subpath, user: user.clone(), domain: domain.clone(), writable,
                };
                table.root.push(racine);
                // Valider **avant** d'écrire quoi que ce soit : un fichier
                // d'identifiants posé pour une source ensuite refusée resterait
                // orphelin sur le disque, avec un mot de passe dedans.
                table.validate().map_err(|e| e.message(&self.catalog.read().unwrap()))?;

                if kind == RootKind::Smb {
                    let r = table.by_name(&name).expect("tout juste inseree");
                    let chemin = r.credentials_path(&self.creds_dir);
                    let secret = if !password.is_empty() {
                        password
                    } else if let Some(c) = self.explore.credentials(&host) {
                        c.password
                    } else {
                        Self::mot_de_passe_existant(&chemin).unwrap_or_default()
                    };
                    Self::ecrire_identifiants(&chemin, &user, &secret, &domain).map_err(|e| {
                        tracing::warn!("writing credentials for {name}: {e}");
                        self.mot("store_io_error").replace("{path}", &chemin.display().to_string())
                    })?;
                }
                self.ecrire_table(&table)?;
                *self.roots.write().await = table;
                // Le montage suit la déclaration : plus de bouton à trouver.
                // Un échec ne défait PAS la déclaration — l'utilisateur perdrait
                // sa saisie à cause d'un NAS endormi — il est rapporté à part.
                *self.mount_error.lock().unwrap() = mount::reconcile(mount::UNIT).await.err();
                Ok(())
            }

            Op::RemoveSource { name } => {
                let mut table = self.roots.read().await.clone();
                let Some(i) = table.root.iter().position(|r| r.name == name) else {
                    return Err(self.mot("unknown_source").replace("{name}", &name));
                };
                let partie = table.root.remove(i);
                self.ecrire_table(&table)?;
                // Le fichier d'identifiants part avec la source : le laisser
                // ferait survivre un mot de passe à ce qui le justifiait.
                let _ = std::fs::remove_file(partie.credentials_path(&self.creds_dir));
                *self.roots.write().await = table;
                *self.mount_error.lock().unwrap() = mount::reconcile(mount::UNIT).await.err();
                Ok(())
            }

            Op::SetWritable { name, writable } => {
                let mut table = self.roots.read().await.clone();
                let Some(r) = table.root.iter_mut().find(|r| r.name == name) else {
                    return Err(self.mot("unknown_source").replace("{name}", &name));
                };
                r.writable = writable;
                self.ecrire_table(&table)?;
                *self.roots.write().await = table;
                *self.mount_error.lock().unwrap() = mount::reconcile(mount::UNIT).await.err();
                Ok(())
            }

            Op::ExploreOpen { kind } => {
                self.explore.ouvrir(kind);
                Ok(())
            }
            Op::ExploreClose => {
                self.explore.fermer();
                Ok(())
            }
            Op::ExploreLocal { path } => self.explore.local(&path),
            Op::SmbConnect { host, user, password, domain } => {
                // Resonder ici : installer le paquet sans redémarrer le service
                // doit donner un résultat juste plutôt qu'un refus périmé.
                self.smb_ok.store(
                    ritornello_plugin_files::smb::available().await,
                    std::sync::atomic::Ordering::Relaxed,
                );
                self.explore.connecter(host, user, password, domain);
                Ok(())
            }
            Op::SmbBrowse { share, path } => {
                self.explore.parcourir(share, path);
                Ok(())
            }
```

Et ajouter la méthode utilitaire dans `impl FilesAdmin` :

```rust
    /// Écrit la table des racines, atomiquement.
    fn ecrire_table(&self, table: &Roots) -> Result<(), String> {
        let texte = toml::to_string_pretty(table).map_err(|e| {
            tracing::warn!("serialising the roots table: {e}");
            self.mot("store_io_error").replace("{path}", &self.roots_path.display().to_string())
        })?;
        let tmp = self.roots_path.with_extension("toml.tmp");
        std::fs::write(&tmp, texte)
            .and_then(|_| std::fs::rename(&tmp, &self.roots_path))
            .map_err(|e| {
                tracing::warn!("saving the roots table: {e}");
                self.mot("store_io_error").replace("{path}", &self.roots_path.display().to_string())
            })
    }
```

- [ ] **Step 6 : Étendre `get_data`**

Dans `get_data`, ajouter avant le `serde_json::json!` final :

```rust
        let volumes = volumes::volumes(&volumes::lire_proc_mounts());
        let mount_error = self.mount_error.lock().unwrap().clone();
        let can_browse_smb = self.smb_ok.load(std::sync::atomic::Ordering::Relaxed);
        let explore = self.explore.vue();
```

et les champs correspondants dans l'objet JSON :

```rust
            "volumes": volumes,
            "can_browse_smb": can_browse_smb,
            "explore": explore,
            "mount_error": mount_error,
```

Ajouter `use ritornello_plugin_files::volumes;` en tête.

- [ ] **Step 7 : Adapter la fabrique de test**

Dans `admin_de_test`, ajouter les trois champs :

```rust
            explore: ritornello_plugin_files::explore::Explorateur::new(
                racine.join("creds"),
                catalogue.clone(),
                smb_ok.clone(),
            ),
            mount_error: Arc::new(Mutex::new(None)),
            smb_ok,
```

en extrayant `catalogue` et `smb_ok` en variables locales avant la construction :

```rust
        let catalogue = Arc::new(RwLock::new(Catalog::load(
            "files", "en", &racine, ritornello_plugin_files::FILES_EN,
        )));
        let smb_ok = Arc::new(std::sync::atomic::AtomicBool::new(false));
```

et en remplaçant le champ `catalog:` par `catalog: catalogue.clone(),`.

- [ ] **Step 8 : Lancer les tests**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test -p ritornello-plugin-files -- --test-threads=1"`
Expected: PASS.

- [ ] **Step 9 : Commit**

```bash
git add crates/ritornello-plugin-files/src/admin.rs
git commit -m "feat(plugin-files): declarer une source d un geste, et monter dans la foulee"
```

---

## Task 8 : `main.rs` — le câblage

**Files:**
- Modify: `crates/ritornello-plugin-files/src/main.rs`

**Interfaces:**
- Consumes: `FilesAdmin` (T7), `smb::available` (T5).
- Produces: rien de nouveau.

- [ ] **Step 1 : Sonder `smbclient` au démarrage et construire l'admin**

Dans la fonction qui construit `FilesAdmin`, avant la construction :

```rust
    // Sonde au démarrage plutôt qu'à l'usage : la page doit pouvoir griser
    // l'assistant réseau dès son ouverture, comme l'onglet Système grise le
    // redémarrage sur `can_reboot`. La sonde est refaite à chaque tentative de
    // connexion, pour qu'installer le paquet sans redémarrer donne un résultat
    // juste.
    let smb_ok = Arc::new(std::sync::atomic::AtomicBool::new(
        ritornello_plugin_files::smb::available().await,
    ));
    if !smb_ok.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::info!("smbclient is not available: the network wizard will be offered read-only");
    }
```

Puis ajouter aux champs de `FilesAdmin` :

```rust
        explore: ritornello_plugin_files::explore::Explorateur::new(
            creds_dir.clone(),
            catalog.clone(),
            smb_ok.clone(),
        ),
        mount_error: Arc::new(Mutex::new(None)),
        smb_ok,
```

- [ ] **Step 2 : Compiler et lancer tous les tests Rust**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings"`
Expected: PASS, aucun avertissement.

- [ ] **Step 3 : Commit**

```bash
git add crates/ritornello-plugin-files/src/main.rs
git commit -m "feat(plugin-files): sonder smbclient au demarrage et cabler les assistants"
```

---

## Task 9 : `donnees.ts` — le contrat côté page

**Files:**
- Modify: `crates/ritornello-plugin-files/ui/src/donnees.ts`
- Modify: `crates/ritornello-plugin-files/ui/src/donnees.test.ts`

**Interfaces:**
- Consumes: la charge utile de T7.
- Produces: `Volume`, `Exploration`, `Donnees` étendu, `normaliser`.

- [ ] **Step 1 : Écrire les tests qui échouent**

Ajouter dans `donnees.test.ts` :

```ts
it('une charge utile sans les champs neufs ne casse pas la page', () => {
  // Le plugin peut être plus ancien que la page pendant un déploiement :
  // absent doit valoir « rien », jamais « undefined » traversant un v-for.
  const d = normaliser({})
  expect(d.volumes).toEqual([])
  expect(d.canBrowseSmb).toBe(false)
  expect(d.explore.open).toBe(false)
  expect(d.explore.dirs).toEqual([])
  expect(d.mountError).toBeNull()
})

it('les volumes et l exploration se relisent', () => {
  const d = normaliser({
    volumes: [{ path: '/media/usb', fstype: 'vfat' }],
    can_browse_smb: true,
    mount_error: 'polkit a refuse',
    explore: {
      open: true, kind: 'smb', host: 'nas', share: 'musique', path: 'Albums',
      shares: ['musique'], dirs: ['Jazz'], audio_count: 12, busy: false, error: null,
    },
  })
  expect(d.volumes[0].path).toBe('/media/usb')
  expect(d.canBrowseSmb).toBe(true)
  expect(d.mountError).toBe('polkit a refuse')
  expect(d.explore.kind).toBe('smb')
  expect(d.explore.audioCount).toBe(12)
  expect(d.explore.dirs).toEqual(['Jazz'])
})
```

- [ ] **Step 2 : Lancer les tests pour vérifier qu'ils échouent**

Run : `npm run test -w ritornello-plugin-files-ui`
Expected: FAIL — `d.volumes is undefined`.

- [ ] **Step 3 : Étendre les types et la normalisation**

Ajouter dans `donnees.ts` :

```ts
export interface Volume {
  path: string
  fstype: string
}

/** L'assistant en cours, tel que le plugin le rapporte. */
export interface Exploration {
  open: boolean
  kind: 'local' | 'smb' | null
  host: string
  share: string
  path: string
  shares: string[]
  dirs: string[]
  audioCount: number
  busy: boolean
  error: string | null
}

const EXPLORATION_VIDE: Exploration = {
  open: false, kind: null, host: '', share: '', path: '',
  shares: [], dirs: [], audioCount: 0, busy: false, error: null,
}
```

Étendre `Donnees` avec :

```ts
  volumes: Volume[]
  canBrowseSmb: boolean
  explore: Exploration
  mountError: string | null
```

et, dans `normaliser`, produire ces champs. Le plugin peut être plus ancien que
la page pendant un déploiement : chaque champ absent vaut sa valeur vide, jamais
`undefined` — un `undefined` traversant un `v-for` casserait le rendu entier
plutôt que d'afficher une section vide.

```ts
  const ex = (brut.explore ?? {}) as Record<string, unknown>
  const explore: Exploration = {
    open: Boolean(ex.open),
    kind: (ex.kind as 'local' | 'smb' | null) ?? null,
    host: String(ex.host ?? ''),
    share: String(ex.share ?? ''),
    path: String(ex.path ?? ''),
    shares: Array.isArray(ex.shares) ? (ex.shares as string[]) : [],
    dirs: Array.isArray(ex.dirs) ? (ex.dirs as string[]) : [],
    audioCount: Number(ex.audio_count ?? 0),
    busy: Boolean(ex.busy),
    error: (ex.error as string | null) ?? null,
  }
```

et les trois autres champs, dans l'objet rendu par `normaliser` :

```ts
  volumes: Array.isArray(brut.volumes)
    ? (brut.volumes as Record<string, unknown>[]).map((v) => ({
        path: String(v.path ?? ''),
        fstype: String(v.fstype ?? ''),
      }))
    : [],
  canBrowseSmb: Boolean(brut.can_browse_smb),
  explore: brut.explore ? explore : EXPLORATION_VIDE,
  mountError: (brut.mount_error as string | null) ?? null,
```

- [ ] **Step 4 : Lancer les tests**

Run : `npm run test -w ritornello-plugin-files-ui`
Expected: PASS.

- [ ] **Step 5 : Commit**

```bash
git add crates/ritornello-plugin-files/ui/src/donnees.ts crates/ritornello-plugin-files/ui/src/donnees.test.ts
git commit -m "feat(plugin-files): le contrat des volumes et de l exploration cote page"
```

---

## Task 10 : `ChoixDossier.vue` — l'arbre de choix partagé

**Files:**
- Create: `crates/ritornello-plugin-files/ui/src/ChoixDossier.vue`
- Create: `crates/ritornello-plugin-files/ui/src/ChoixDossier.test.ts`

**Interfaces:**
- Consumes: `Exploration` (T9).
- Produces: composant à props `{ exploration: Exploration; t: T; fige: boolean }`, émettant `descendre(nom: string)`, `remonter()`.

- [ ] **Step 1 : Écrire les tests qui échouent**

```ts
import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import ChoixDossier from './ChoixDossier.vue'
import { t } from './harnais'

const base = {
  open: true, kind: 'local' as const, host: '', share: '', path: '/media/usb',
  shares: [], dirs: ['Albums', 'Live'], audioCount: 3, busy: false, error: null,
}

describe('ChoixDossier', () => {
  it('liste les sous-dossiers et annonce le compte audio', () => {
    // Le compte est ce qui dit qu'on est au bon endroit : sans lui on choisit
    // un dossier en espérant.
    const w = mount(ChoixDossier, { props: { exploration: base, t, fige: false } })
    expect(w.findAll('[data-choix-dossier]')).toHaveLength(2)
    expect(w.get('[data-audio-count]').text()).toContain('3')
  })

  it('descendre emet le nom du dossier, pas un chemin', () => {
    // C'est l'appelant qui sait composer le chemin : local et SMB ne les
    // composent pas de la meme facon.
    const w = mount(ChoixDossier, { props: { exploration: base, t, fige: false } })
    w.findAll('[data-choix-dossier]')[1].trigger('click')
    expect(w.emitted('descendre')?.[0]).toEqual(['Live'])
  })

  it('un dossier vide le dit au lieu de ne rien afficher', () => {
    // Une liste vide sans phrase se lit comme un chargement qui n'a pas fini.
    const w = mount(ChoixDossier, {
      props: { exploration: { ...base, dirs: [] }, t, fige: false },
    })
    expect(w.find('[data-choix-vide]').exists()).toBe(true)
  })

  it('pendant une attente rien n est cliquable et l attente se voit', () => {
    const w = mount(ChoixDossier, {
      props: { exploration: { ...base, busy: true }, t, fige: false },
    })
    expect(w.find('[data-choix-busy]').exists()).toBe(true)
    expect(w.findAll('[data-choix-dossier]')[0].attributes('disabled')).toBeDefined()
  })

  it('un refus s affiche a la place de l arbre', () => {
    // Afficher un arbre vide sous un message d'erreur laisserait croire que le
    // dossier existe et qu'il est vide.
    const w = mount(ChoixDossier, {
      props: { exploration: { ...base, error: 'hote injoignable' }, t, fige: false },
    })
    expect(w.get('[data-choix-erreur]').text()).toContain('hote injoignable')
    expect(w.findAll('[data-choix-dossier]')).toHaveLength(0)
  })
})
```

- [ ] **Step 2 : Lancer les tests pour vérifier qu'ils échouent**

Run : `npm run test -w ritornello-plugin-files-ui -- ChoixDossier`
Expected: FAIL — fichier introuvable.

- [ ] **Step 3 : Écrire le composant**

```vue
<script setup lang="ts">
import { Button } from '@ritornello/ui'
import type { Exploration, T } from './donnees'

/**
 * L'arbre de choix des deux assistants.
 *
 * Partagé, parce que descendre dans des dossiers est le même geste des deux
 * côtés : seule la machine qui répond change. Il n'émet que des **noms** de
 * dossier, jamais des chemins — la composition du chemin appartient à
 * l'appelant, un chemin local et un chemin SMB ne se composant pas pareil.
 */
const props = defineProps<{ exploration: Exploration; t: T; fige: boolean }>()
defineEmits<{ descendre: [nom: string]; remonter: [] }>()
</script>

<template>
  <div class="space-y-2" data-choix>
    <div class="flex items-center gap-2 text-sm">
      <Button
        variant="ghost"
        size="sm"
        data-choix-remonter
        :disabled="fige || exploration.busy"
        @click="$emit('remonter')"
      >
        ↑ {{ t('btn_up') }}
      </Button>
      <span class="truncate text-muted-foreground" data-choix-chemin>
        {{ exploration.path || '/' }}
      </span>
    </div>

    <!-- Le refus remplace l'arbre : l'afficher vide en dessous laisserait
         croire que le dossier existe et qu'il est vide. -->
    <p v-if="exploration.error" class="text-sm text-destructive" data-choix-erreur>
      {{ exploration.error }}
    </p>

    <template v-else>
      <p v-if="exploration.busy" class="text-sm text-muted-foreground" data-choix-busy>
        {{ t('connecting') }}
      </p>

      <ul class="max-h-64 space-y-1 overflow-y-auto text-sm">
        <li v-for="d in exploration.dirs" :key="d">
          <button
            type="button"
            data-choix-dossier
            class="w-full truncate rounded px-2 py-1 text-left hover:bg-accent"
            :disabled="fige || exploration.busy"
            @click="$emit('descendre', d)"
          >
            📁 {{ d }}
          </button>
        </li>
        <li
          v-if="!exploration.dirs.length && !exploration.busy"
          class="px-2 text-muted-foreground"
          data-choix-vide
        >
          {{ t('empty_folder') }}
        </li>
      </ul>

      <!-- Le compte de fichiers audio du niveau ouvert : c'est lui qui dit
           qu'on est au bon endroit. -->
      <p class="text-sm text-muted-foreground" data-audio-count>
        {{ t('audio_here', { count: exploration.audioCount }) }}
      </p>
    </template>
  </div>
</template>
```

- [ ] **Step 4 : Lancer les tests**

Run : `npm run test -w ritornello-plugin-files-ui -- ChoixDossier`
Expected: PASS, 5 tests.

- [ ] **Step 5 : Commit**

```bash
git add crates/ritornello-plugin-files/ui/src/ChoixDossier.vue crates/ritornello-plugin-files/ui/src/ChoixDossier.test.ts
git commit -m "feat(plugin-files): l arbre de choix, partage par les deux assistants"
```

---

## Task 11 : `DialogueAppareil.vue`

**Files:**
- Create: `crates/ritornello-plugin-files/ui/src/DialogueAppareil.vue`
- Create: `crates/ritornello-plugin-files/ui/src/DialogueAppareil.test.ts`

**Interfaces:**
- Consumes: `ChoixDossier` (T10), `Donnees`, `Envoyer` (T9).
- Produces: props `{ donnees: Donnees; t: T; envoyer: Envoyer; fige: boolean; ouvert: boolean }`, émet `fermer()`.

- [ ] **Step 1 : Écrire les tests qui échouent**

```ts
import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import DialogueAppareil from './DialogueAppareil.vue'
import { donneesDeTest, t } from './harnais'

function monter(surcharges = {}) {
  const envoyer = vi.fn().mockResolvedValue(donneesDeTest())
  const donnees = { ...donneesDeTest(), volumes: [{ path: '/media/usb', fstype: 'vfat' }], ...surcharges }
  const w = mount(DialogueAppareil, { props: { donnees, t, envoyer, fige: false, ouvert: true } })
  return { w, envoyer }
}

describe('DialogueAppareil', () => {
  it('propose les volumes montes', () => {
    const { w } = monter()
    expect(w.get('[data-volume]').text()).toContain('/media/usb')
  })

  it('choisir un volume demande son contenu au plugin', () => {
    const { w, envoyer } = monter()
    w.get('[data-volume]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb' })
  })

  it('descendre compose le chemin absolu', () => {
    // Le composant d'arbre n'emet qu'un nom : c'est ici que le chemin se
    // compose, et un chemin local se compose avec des barres obliques.
    const { w, envoyer } = monter({
      explore: { ...donneesDeTest().explore, open: true, kind: 'local', path: '/media/usb', dirs: ['Albums'] },
    })
    w.get('[data-choix-dossier]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_local', path: '/media/usb/Albums' })
  })

  it('confirmer declare la source avec le chemin courant', () => {
    const { w, envoyer } = monter({
      explore: { ...donneesDeTest().explore, open: true, kind: 'local', path: '/media/usb/Albums', dirs: [] },
    })
    w.get('[data-choisir]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({
      op: 'add_source', kind: 'local', path: '/media/usb/Albums',
      host: '', share: '', subpath: null, user: '', domain: '', password: '', writable: false,
    })
  })

  it('sans volume la popin le dit au lieu d offrir une liste vide', () => {
    const { w } = monter({ volumes: [] })
    expect(w.find('[data-no-volumes]').exists()).toBe(true)
  })
})
```

- [ ] **Step 2 : Lancer les tests pour vérifier qu'ils échouent**

Run : `npm run test -w ritornello-plugin-files-ui -- DialogueAppareil`
Expected: FAIL — fichier introuvable.

- [ ] **Step 3 : Écrire le composant**

```vue
<script setup lang="ts">
import { Button, Dialog, DialogContent, DialogHeader, DialogTitle } from '@ritornello/ui'
import { computed } from 'vue'
import ChoixDossier from './ChoixDossier.vue'
import type { Donnees, Envoyer, T } from './donnees'

/**
 * Assistant « dossier de l'appareil ».
 *
 * Il ouvre sur la liste des volumes, jamais sur `/` : le chemin absolu d'une
 * clé USB n'est connu de personne, et c'est précisément ce que l'ancien
 * formulaire demandait de taper.
 */
const props = defineProps<{
  donnees: Donnees
  t: T
  envoyer: Envoyer
  fige: boolean
  ouvert: boolean
}>()
const emit = defineEmits<{ fermer: [] }>()

const ex = computed(() => props.donnees.explore)
/** Un volume a été choisi : on est dans l'arbre plutôt que dans la liste. */
const dansLArbre = computed(() => ex.value.kind === 'local' && ex.value.path !== '')

function aller(chemin: string): void {
  void props.envoyer({ op: 'explore_local', path: chemin })
}

function descendre(nom: string): void {
  // Le chemin se compose ici : l'arbre n'émet que des noms, parce qu'un chemin
  // local et un chemin SMB ne se composent pas de la même façon.
  aller(`${ex.value.path.replace(/\/$/, '')}/${nom}`)
}

function remonter(): void {
  const parent = ex.value.path.replace(/\/[^/]+\/?$/, '')
  aller(parent || '/')
}

async function choisir(): Promise<void> {
  const ok = await props.envoyer({
    op: 'add_source',
    kind: 'local',
    path: ex.value.path,
    host: '',
    share: '',
    subpath: null,
    user: '',
    domain: '',
    password: '',
    writable: false,
  })
  if (ok) fermer()
}

function fermer(): void {
  void props.envoyer({ op: 'explore_close' })
  emit('fermer')
}
</script>

<template>
  <Dialog :open="ouvert" @update:open="(v: boolean) => !v && fermer()">
    <DialogContent data-dlg-appareil>
      <DialogHeader>
        <DialogTitle>{{ t('dlg_device_title') }}</DialogTitle>
      </DialogHeader>

      <div v-if="!dansLArbre" class="space-y-2">
        <p class="text-sm text-muted-foreground">{{ t('volumes_label') }}</p>
        <p v-if="!donnees.volumes.length" class="text-sm text-muted-foreground" data-no-volumes>
          {{ t('no_volumes') }}
        </p>
        <button
          v-for="v in donnees.volumes"
          :key="v.path"
          type="button"
          data-volume
          class="flex w-full items-center gap-2 rounded px-2 py-1 text-left text-sm hover:bg-accent"
          :disabled="fige"
          @click="aller(v.path)"
        >
          <span class="flex-1 truncate">{{ v.path }}</span>
          <span class="text-xs text-muted-foreground">{{ v.fstype }}</span>
        </button>
      </div>

      <ChoixDossier
        v-else
        :exploration="ex"
        :t="t"
        :fige="fige"
        @descendre="descendre"
        @remonter="remonter"
      />

      <div class="flex justify-end gap-2">
        <Button variant="ghost" data-annuler @click="fermer">{{ t('btn_cancel') }}</Button>
        <Button data-choisir :disabled="fige || !dansLArbre" @click="choisir">
          {{ t('btn_choose_folder') }}
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
```

- [ ] **Step 4 : Lancer les tests**

Run : `npm run test -w ritornello-plugin-files-ui -- DialogueAppareil`
Expected: PASS, 5 tests.

- [ ] **Step 5 : Commit**

```bash
git add crates/ritornello-plugin-files/ui/src/DialogueAppareil.vue crates/ritornello-plugin-files/ui/src/DialogueAppareil.test.ts
git commit -m "feat(plugin-files): l assistant de dossier local, qui ouvre sur les volumes"
```

---

## Task 12 : `DialoguePartage.vue`

**Files:**
- Create: `crates/ritornello-plugin-files/ui/src/DialoguePartage.vue`
- Create: `crates/ritornello-plugin-files/ui/src/DialoguePartage.test.ts`

**Interfaces:**
- Consumes: `ChoixDossier` (T10), `Donnees`, `Envoyer` (T9).
- Produces: mêmes props que T11.

- [ ] **Step 1 : Écrire les tests qui échouent**

```ts
import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import DialoguePartage from './DialoguePartage.vue'
import { donneesDeTest, t } from './harnais'

function monter(surcharges = {}) {
  const envoyer = vi.fn().mockResolvedValue(donneesDeTest())
  const donnees = { ...donneesDeTest(), canBrowseSmb: true, ...surcharges }
  const w = mount(DialoguePartage, { props: { donnees, t, envoyer, fige: false, ouvert: true } })
  return { w, envoyer }
}

describe('DialoguePartage', () => {
  it('se connecter envoie l hote et les identifiants une seule fois', async () => {
    const { w, envoyer } = monter()
    await w.get('[data-host]').setValue('192.168.1.20')
    await w.get('[data-user]').setValue('steven')
    await w.get('[data-password]').setValue('secret')
    await w.get('[data-connect]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({
      op: 'smb_connect', host: '192.168.1.20', user: 'steven', password: 'secret', domain: '',
    })
  })

  it('choisir un partage demande sa racine', () => {
    const { w, envoyer } = monter({
      explore: { ...donneesDeTest().explore, open: true, kind: 'smb', host: 'nas', shares: ['musique'] },
    })
    w.get('[data-share]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({ op: 'smb_browse', share: 'musique', path: '' })
  })

  it('confirmer declare la source sans reclamer le mot de passe', () => {
    // Il vient de servir a se connecter : le faire retaper serait absurde, et
    // la page ne l'a de toute facon jamais recu en retour.
    const { w, envoyer } = monter({
      explore: {
        ...donneesDeTest().explore, open: true, kind: 'smb', host: 'nas',
        share: 'musique', path: 'Ma Musique', shares: ['musique'], dirs: [],
      },
    })
    w.get('[data-choisir]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({
      op: 'add_source', kind: 'smb', path: null, host: 'nas', share: 'musique',
      subpath: 'Ma Musique', user: '', domain: '', password: '', writable: false,
    })
  })

  it('sans smbclient l assistant est grise et la raison est nommee', () => {
    // Comme l onglet Systeme grise le redemarrage : jamais de plantage, jamais
    // un bouton qui echoue sans dire pourquoi.
    const { w } = monter({ canBrowseSmb: false })
    expect(w.get('[data-smb-unavailable]').text().length).toBeGreaterThan(0)
    expect(w.get('[data-connect]').attributes('disabled')).toBeDefined()
  })

  it('le repli manuel reste offert sans smbclient', () => {
    // Sans lui, ce chantier RETIRERAIT une capacite qui existe aujourd hui.
    const { w, envoyer } = monter({ canBrowseSmb: false })
    w.get('[data-manuel]').trigger('click')
    expect(w.find('[data-manual-share]').exists()).toBe(true)
  })

  it('la saisie manuelle declare la source directement', async () => {
    const { w, envoyer } = monter({ canBrowseSmb: false })
    await w.get('[data-manuel]').trigger('click')
    await w.get('[data-host]').setValue('nas')
    await w.get('[data-manual-share]').setValue('musique')
    await w.get('[data-manual-subpath]').setValue('Albums')
    await w.get('[data-user]').setValue('steven')
    await w.get('[data-password]').setValue('secret')
    await w.get('[data-choisir]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({
      op: 'add_source', kind: 'smb', path: null, host: 'nas', share: 'musique',
      subpath: 'Albums', user: 'steven', domain: '', password: 'secret', writable: false,
    })
  })
})
```

- [ ] **Step 2 : Lancer les tests pour vérifier qu'ils échouent**

Run : `npm run test -w ritornello-plugin-files-ui -- DialoguePartage`
Expected: FAIL — fichier introuvable.

- [ ] **Step 3 : Écrire le composant**

```vue
<script setup lang="ts">
import { Button, Dialog, DialogContent, DialogHeader, DialogTitle, Input } from '@ritornello/ui'
import { computed, ref } from 'vue'
import ChoixDossier from './ChoixDossier.vue'
import type { Donnees, Envoyer, T } from './donnees'

/**
 * Assistant « partage réseau ».
 *
 * Trois temps : hôte, puis partages, puis dossiers. Le mode manuel n'est pas un
 * repli honteux : il sert quand `smbclient` manque, et sans lui ce chantier
 * retirerait une capacité qui existe aujourd'hui.
 */
const props = defineProps<{
  donnees: Donnees
  t: T
  envoyer: Envoyer
  fige: boolean
  ouvert: boolean
}>()
const emit = defineEmits<{ fermer: [] }>()

const host = ref('')
const user = ref('')
const password = ref('')
const domain = ref('')
const manuel = ref(false)
const partageManuel = ref('')
const sousCheminManuel = ref('')

const ex = computed(() => props.donnees.explore)
const dansLArbre = computed(() => ex.value.kind === 'smb' && ex.value.share !== '')
const listeDePartages = computed(() => ex.value.shares.length > 0 && !dansLArbre.value)

function connecter(): void {
  void props.envoyer({
    op: 'smb_connect',
    host: host.value.trim(),
    user: user.value.trim(),
    password: password.value,
    domain: domain.value.trim(),
  })
}

function choisirPartage(nom: string): void {
  void props.envoyer({ op: 'smb_browse', share: nom, path: '' })
}

function descendre(nom: string): void {
  const suite = ex.value.path ? `${ex.value.path}/${nom}` : nom
  void props.envoyer({ op: 'smb_browse', share: ex.value.share, path: suite })
}

function remonter(): void {
  void props.envoyer({
    op: 'smb_browse',
    share: ex.value.share,
    path: ex.value.path.replace(/\/?[^/]+$/, ''),
  })
}

async function choisir(): Promise<void> {
  // En mode manuel, tout vient des champs ; en mode assistant, tout vient de
  // ce qu'on a parcouru — et le mot de passe reste vide, parce qu'il vit déjà
  // dans la session du plugin et que la page ne l'a jamais reçu en retour.
  const charge = manuel.value
    ? {
        host: host.value.trim(),
        share: partageManuel.value.trim(),
        subpath: sousCheminManuel.value.trim() || null,
        user: user.value.trim(),
        domain: domain.value.trim(),
        password: password.value,
      }
    : {
        host: ex.value.host,
        share: ex.value.share,
        subpath: ex.value.path || null,
        user: '',
        domain: '',
        password: '',
      }
  const ok = await props.envoyer({ op: 'add_source', kind: 'smb', path: null, writable: false, ...charge })
  if (ok) fermer()
}

function fermer(): void {
  void props.envoyer({ op: 'explore_close' })
  emit('fermer')
}
</script>

<template>
  <Dialog :open="ouvert" @update:open="(v: boolean) => !v && fermer()">
    <DialogContent data-dlg-partage>
      <DialogHeader>
        <DialogTitle>{{ t('dlg_share_title') }}</DialogTitle>
      </DialogHeader>

      <!-- Grisé, jamais planté : c'est la convention de l'onglet Système. -->
      <p
        v-if="!donnees.canBrowseSmb"
        class="text-sm text-muted-foreground"
        data-smb-unavailable
      >
        {{ t('smb_unavailable') }}
      </p>

      <div class="flex flex-wrap gap-2">
        <Input v-model="host" data-host class="w-44" :placeholder="t('ph_host')" />
        <Input v-model="user" data-user class="w-32" :placeholder="t('ph_user')" />
        <Input
          v-model="password"
          type="password"
          data-password
          class="w-32"
          :placeholder="t('ph_password')"
        />
        <Input v-model="domain" data-domain class="w-28" :placeholder="t('ph_domain')" />
      </div>

      <div v-if="manuel" class="flex flex-wrap gap-2">
        <Input
          v-model="partageManuel"
          data-manual-share
          class="w-40"
          :placeholder="t('ph_share')"
        />
        <Input
          v-model="sousCheminManuel"
          data-manual-subpath
          class="w-40"
          :placeholder="t('ph_subpath')"
        />
      </div>

      <template v-else>
        <p v-if="ex.error" class="text-sm text-destructive" data-partage-erreur>{{ ex.error }}</p>

        <div v-if="listeDePartages" class="space-y-1">
          <p class="text-sm text-muted-foreground">{{ t('shares_label') }}</p>
          <button
            v-for="s in ex.shares"
            :key="s"
            type="button"
            data-share
            class="w-full truncate rounded px-2 py-1 text-left text-sm hover:bg-accent"
            :disabled="fige || ex.busy"
            @click="choisirPartage(s)"
          >
            {{ s }}
          </button>
        </div>

        <ChoixDossier
          v-else-if="dansLArbre"
          :exploration="ex"
          :t="t"
          :fige="fige"
          @descendre="descendre"
          @remonter="remonter"
        />
      </template>

      <div class="flex flex-wrap justify-end gap-2">
        <Button variant="ghost" data-manuel @click="manuel = !manuel">
          {{ manuel ? t('btn_assistant') : t('btn_manual') }}
        </Button>
        <Button variant="ghost" data-annuler @click="fermer">{{ t('btn_cancel') }}</Button>
        <Button
          v-if="!manuel"
          variant="secondary"
          data-connect
          :disabled="fige || !donnees.canBrowseSmb || ex.busy"
          @click="connecter"
        >
          {{ ex.busy ? t('connecting') : t('btn_connect') }}
        </Button>
        <Button data-choisir :disabled="fige || (!manuel && !dansLArbre)" @click="choisir">
          {{ t('btn_choose_folder') }}
        </Button>
      </div>
    </DialogContent>
  </Dialog>
</template>
```

- [ ] **Step 4 : Lancer les tests**

Run : `npm run test -w ritornello-plugin-files-ui -- DialoguePartage`
Expected: PASS, 6 tests.

- [ ] **Step 5 : Commit**

```bash
git add crates/ritornello-plugin-files/ui/src/DialoguePartage.vue crates/ritornello-plugin-files/ui/src/DialoguePartage.test.ts
git commit -m "feat(plugin-files): l assistant de partage reseau, et son repli manuel"
```

---

## Task 13 : `VoletSources.vue`

**Files:**
- Create: `crates/ritornello-plugin-files/ui/src/VoletSources.vue`
- Create: `crates/ritornello-plugin-files/ui/src/VoletSources.test.ts`
- Delete: `crates/ritornello-plugin-files/ui/src/VoletRacines.vue`
- Delete: `crates/ritornello-plugin-files/ui/src/VoletRacines.test.ts`

**Interfaces:**
- Consumes: `DialogueAppareil` (T11), `DialoguePartage` (T12).
- Produces: composant à props `{ donnees; t; envoyer; fige }`.

- [ ] **Step 1 : Écrire les tests qui échouent**

```ts
import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import VoletSources from './VoletSources.vue'
import { donneesDeTest, racineDeTest, t } from './harnais'

function monter(surcharges = {}) {
  const envoyer = vi.fn().mockResolvedValue(donneesDeTest())
  const donnees = { ...donneesDeTest(), ...surcharges }
  const w = mount(VoletSources, { props: { donnees, t, envoyer, fige: false } })
  return { w, envoyer }
}

describe('VoletSources', () => {
  it('sans source la page invite a en ajouter une', () => {
    const { w } = monter({ roots: [] })
    expect(w.find('[data-no-sources]').exists()).toBe(true)
  })

  it('chaque source offre l ajout de tout son contenu a la liste', () => {
    // La demande explicite : depuis les sources declarees, ajouter tout doit
    // etre a un clic.
    const { w, envoyer } = monter({ roots: [racineDeTest({ name: 'usb' })] })
    w.get('[data-add-all]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({ op: 'add_dir', root: 'usb', path: '' })
  })

  it('retirer une source la nomme au plugin', () => {
    const { w, envoyer } = monter({ roots: [racineDeTest({ name: 'usb' })] })
    w.get('[data-remove-source]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({ op: 'remove_source', name: 'usb' })
  })

  it('basculer l inscriptibilite est une operation a part', () => {
    // Sinon changer d avis imposerait de retirer puis redeclarer, donc de
    // resaisir le mot de passe.
    const { w, envoyer } = monter({
      roots: [racineDeTest({ name: 'nas', kind: 'smb', writable: false })],
    })
    w.get('[data-writable]').setValue(true)
    expect(envoyer).toHaveBeenCalledWith({ op: 'set_writable', name: 'nas', writable: true })
  })

  it('un echec de montage se voit et se reessaie', () => {
    // Le montage suit desormais la declaration : sans ce rapport, une source
    // resterait non montee sans jamais dire pourquoi.
    const { w, envoyer } = monter({
      roots: [racineDeTest({ name: 'nas', kind: 'smb', mounted: false })],
      mountError: 'Interactive authentication required.',
    })
    expect(w.get('[data-mount-error]').text()).toContain('Interactive authentication')
    w.get('[data-retry-mount]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({ op: 'mount' })
  })

  it('les deux boutons ouvrent chacun leur assistant', async () => {
    const { w, envoyer } = monter()
    await w.get('[data-add-device]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_open', kind: 'local' })
    await w.get('[data-add-share]').trigger('click')
    expect(envoyer).toHaveBeenCalledWith({ op: 'explore_open', kind: 'smb' })
  })
})
```

Ajouter dans `harnais.ts` une fabrique `racineDeTest(surcharges)` rendant une
`Racine` complète, et étendre `donneesDeTest()` avec `volumes: []`,
`canBrowseSmb: false`, `mountError: null` et une `explore` vide.

- [ ] **Step 2 : Lancer les tests pour vérifier qu'ils échouent**

Run : `npm run test -w ritornello-plugin-files-ui -- VoletSources`
Expected: FAIL — fichier introuvable.

- [ ] **Step 3 : Écrire le composant**

```vue
<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { ref } from 'vue'
import DialogueAppareil from './DialogueAppareil.vue'
import DialoguePartage from './DialoguePartage.vue'
import { cibleRacine, type Donnees, type Envoyer, type T } from './donnees'

/**
 * Les sources déclarées.
 *
 * Plus de formulaire : on ne tape plus une adresse à l'aveugle, on parcourt
 * puis on déclare. Ce volet ne fait donc qu'énumérer ce qui existe et ouvrir
 * l'un des deux assistants.
 */
const props = defineProps<{ donnees: Donnees; t: T; envoyer: Envoyer; fige: boolean }>()

const appareilOuvert = ref(false)
const partageOuvert = ref(false)

function ouvrir(kind: 'local' | 'smb'): void {
  void props.envoyer({ op: 'explore_open', kind })
  if (kind === 'local') appareilOuvert.value = true
  else partageOuvert.value = true
}

function toutAjouter(nom: string): void {
  void props.envoyer({ op: 'add_dir', root: nom, path: '' })
}

function retirer(nom: string): void {
  void props.envoyer({ op: 'remove_source', name: nom })
}

function basculer(nom: string, writable: boolean): void {
  void props.envoyer({ op: 'set_writable', name: nom, writable })
}

function remonter(): void {
  void props.envoyer({ op: 'mount' })
}
</script>

<template>
  <section class="space-y-4" data-volet-sources>
    <h2 class="font-medium">{{ t('sources_title') }}</h2>

    <p v-if="!donnees.roots.length" class="text-sm text-muted-foreground" data-no-sources>
      {{ t('no_sources') }}
    </p>

    <div
      v-for="r in donnees.roots"
      :key="r.name"
      data-source
      class="flex flex-wrap items-center gap-2 rounded-md border border-border p-3"
    >
      <span class="text-xs text-muted-foreground" data-source-kind>
        {{ r.kind === 'local' ? t('kind_local') : t('kind_smb') }}
      </span>
      <span class="flex-1 truncate text-sm" data-source-target>{{ cibleRacine(r) }}</span>

      <!-- L'état du montage est **observé**, jamais saisi : il vient du plugin,
           qui regarde le système de fichiers. -->
      <span v-if="r.kind === 'smb'" class="text-xs" data-source-mounted>
        {{ r.mounted ? t('mounted_yes') : t('mounted_no') }}
      </span>

      <label v-if="r.kind === 'smb'" class="flex items-center gap-1 text-sm">
        <input
          type="checkbox"
          data-writable
          :checked="r.writable"
          :disabled="fige"
          @change="basculer(r.name, ($event.target as HTMLInputElement).checked)"
        />
        {{ t('writable_label') }}
      </label>

      <Button variant="secondary" size="sm" data-add-all :disabled="fige" @click="toutAjouter(r.name)">
        {{ t('btn_add_to_playlist') }}
      </Button>
      <Button
        variant="ghost"
        size="sm"
        data-remove-source
        :aria-label="t('btn_remove_source')"
        :disabled="fige"
        @click="retirer(r.name)"
      >
        ✕
      </Button>
    </div>

    <!-- Le montage suit désormais la déclaration : sans ce rapport, une source
         resterait « non montée » sans jamais dire pourquoi. -->
    <div v-if="donnees.mountError" class="space-y-1" data-mount-error>
      <p class="text-sm text-destructive">{{ t('mount_error_title') }} {{ donnees.mountError }}</p>
      <Button variant="outline" size="sm" data-retry-mount :disabled="fige" @click="remonter">
        {{ t('btn_retry_mount') }}
      </Button>
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <Button variant="secondary" data-add-device :disabled="fige" @click="ouvrir('local')">
        {{ t('btn_add_device') }}
      </Button>
      <Button variant="secondary" data-add-share :disabled="fige" @click="ouvrir('smb')">
        {{ t('btn_add_share') }}
      </Button>
    </div>

    <DialogueAppareil
      :donnees="donnees"
      :t="t"
      :envoyer="envoyer"
      :fige="fige"
      :ouvert="appareilOuvert"
      @fermer="appareilOuvert = false"
    />
    <DialoguePartage
      :donnees="donnees"
      :t="t"
      :envoyer="envoyer"
      :fige="fige"
      :ouvert="partageOuvert"
      @fermer="partageOuvert = false"
    />
  </section>
</template>
```

- [ ] **Step 4 : Supprimer l'ancien volet**

```bash
git rm crates/ritornello-plugin-files/ui/src/VoletRacines.vue crates/ritornello-plugin-files/ui/src/VoletRacines.test.ts
```

- [ ] **Step 5 : Lancer les tests**

Run : `npm run test -w ritornello-plugin-files-ui`
Expected: PASS.

- [ ] **Step 6 : Commit**

```bash
git add -A crates/ritornello-plugin-files/ui/src
git commit -m "feat(plugin-files): les sources declarees remplacent le formulaire de racines"
```

---

## Task 14 : `FilesAdmin.vue` et harmonisation

**Files:**
- Modify: `crates/ritornello-plugin-files/ui/src/FilesAdmin.vue`
- Modify: `crates/ritornello-plugin-files/ui/src/FilesAdmin.test.ts`
- Modify: `crates/ritornello-plugin-files/ui/src/VoletParcourir.vue`
- Modify: `crates/ritornello-plugin-files/ui/src/i18nKeysUsed.test.ts`

**Interfaces:**
- Consumes: `VoletSources` (T13).
- Produces: la page assemblée.

- [ ] **Step 1 : Remplacer le volet dans la page**

Dans `FilesAdmin.vue`, remplacer l'import et l'usage de `VoletRacines` par
`VoletSources` (mêmes props).

- [ ] **Step 2 : Harmoniser les libellés d'ajout**

Dans `VoletParcourir.vue`, remplacer les trois occurrences de `t('btn_add_dir')`
par `t('btn_add_to_playlist')`, et `t('btn_add_file')` par
`t('btn_add_to_playlist')` sur la rangée de fichier.

Le geste est le même partout — ajouter à la liste — et deux libellés différents
pour un même geste font hésiter.

Retirer le bloc « ajouter la racine entière » (`data-add-root-dir`) : il fait
désormais doublon avec le « Ajouter à la liste » de chaque ligne de source.

- [ ] **Step 3 : Écrire le test d'assemblage**

Ajouter dans `FilesAdmin.test.ts` :

```ts
it('la page monte les trois volets', () => {
  const w = monterPage()
  expect(w.find('[data-volet-sources]').exists()).toBe(true)
  expect(w.find('[data-volet-parcourir]').exists()).toBe(true)
  expect(w.find('[data-volet-liste]').exists()).toBe(true)
})
```

Adapter les tests existants qui cherchaient `[data-volet-racines]`.

- [ ] **Step 4 : Vider la liste d'attente i18n**

Dans `i18nKeysUsed.test.ts`, vérifier que `EN_ATTENTE` est vide. Toutes les clés
neuves ont été posées en Tâche 1 : le test doit passer sans ajout.

- [ ] **Step 5 : Lancer tous les tests de page**

Run : `npm run test -w ritornello-plugin-files-ui && npm run typecheck -w ritornello-plugin-files-ui`
Expected: PASS, aucun avertissement de `vue-tsc`.

- [ ] **Step 6 : Commit**

```bash
git add crates/ritornello-plugin-files/ui/src
git commit -m "feat(plugin-files): assembler la page et nommer l ajout pareil partout"
```

---

## Task 15 : La documentation

**Files:**
- Modify: `docs/plugins.md`
- Modify: `docs/installation.md`

**Interfaces:** aucune dépendance de code — exécutable en parallèle des tâches 2 à 5.

- [ ] **Step 1 : Ajouter les prérequis paquets dans `docs/plugins.md`**

Dans la section `## ritornello-plugin-files`, insérer avant la description des
opérations :

```markdown
### Prérequis paquets

Deux paquets système, dont un seul est indispensable.

| Paquet | Rôle | Sans lui |
|---|---|---|
| `cifs-utils` | monter un partage SMB | Une source réseau se déclare mais ne se monte pas : le plugin rapporte l'erreur de `mount`, et la source reste « non montée ». Les dossiers de l'appareil ne sont pas concernés. |
| `smbclient` | parcourir un partage **avant** de le monter | L'assistant réseau est grisé et le dit. Un partage se déclare encore en saisissant hôte, partage et sous-chemin à la main. Rien d'autre n'est affecté. |

```sh
sudo apt install cifs-utils smbclient
```

Le plugin **sonde** la présence de `smbclient` au démarrage et la réexpose dans
`can_browse_smb`. La page grise l'assistant plutôt que d'échouer au clic, comme
l'onglet Système grise le redémarrage quand logind le refuse. La sonde est
refaite à chaque tentative de connexion : installer le paquet sans redémarrer le
service donne un résultat juste.
```

- [ ] **Step 2 : Décrire les deux assistants**

Ajouter, toujours dans la section du plugin :

```markdown
### Déclarer une source

On ne saisit plus d'adresse à l'aveugle : on parcourt, puis on déclare.

- **Dossier de l'appareil** — la popin ouvre sur les volumes montés, lus dans
  `/proc/mounts` et filtrés par une liste blanche de systèmes de fichiers. Les
  pseudo-systèmes de fichiers (`/proc`, `/sys`, `/run`, `/dev`) ne sont ni
  proposés ni parcourables : sans cette borne, un « ajouter à la liste » lancé
  sur `/` partirait dans les liens récursifs de `/proc/self`.
- **Partage réseau** — on saisit une adresse, on se connecte, `smbclient`
  énumère les partages, puis on descend dans les dossiers. Rien n'est monté tant
  qu'on n'a pas confirmé.

Le nom technique de la racine est **dérivé** du partage ou du dernier segment du
chemin, et dédoublonné. Il n'est plus saisi : il devient un composant de
`/mnt/ritornello/<nom>` et un nom de fichier d'identifiants, et la dérivation le
produit conforme par construction.

Le montage **suit la déclaration** : le plugin demande la réconciliation
lui-même. Un échec ne défait pas la déclaration — on perdrait la saisie à cause
d'un NAS endormi — il est rapporté sur la page avec un bouton de réessai.
```

- [ ] **Step 3 : Reprendre la liste dans `docs/installation.md`**

Dans la section des partages réseau, remplacer la mention isolée de
`cifs-utils` par la ligne complète `sudo apt install cifs-utils smbclient`, en
renvoyant à `docs/plugins.md` pour ce que chacun dégrade.

- [ ] **Step 4 : Commit**

```bash
git add docs/plugins.md docs/installation.md
git commit -m "docs(plugin-files): les prerequis paquets, et ce que leur absence degrade"
```

---

## Task 16 : Le parcours de bout en bout

**Files:**
- Modify: `web/app/e2e/serve.mjs`
- Modify: `web/app/e2e/files.spec.ts`
- Create: `web/app/e2e/faux-smbclient.sh`

**Interfaces:**
- Consumes: tout le reste.
- Produces: la garantie que les deux assistants fonctionnent assemblés.

- [ ] **Step 1 : Décrire des volumes sans en monter**

Dans `serve.mjs`, écrire un `/proc/mounts` de fixture dans le répertoire de
travail du parcours et exporter `RITORNELLO_FILES_PROC_MOUNTS` vers lui :

```js
// Le parcours n'a aucun privilège : il ne peut rien monter. Il décrit donc les
// volumes plutôt que de les créer, grâce à la surcharge que le plugin accepte.
const mountsFixture = path.join(dossierEtat, 'proc-mounts')
fs.writeFileSync(mountsFixture, `/dev/sda1 ${mediaRoot} ext4 rw,relatime 0 0\nproc /proc proc rw 0 0\n`)
env.RITORNELLO_FILES_PROC_MOUNTS = mountsFixture
```

- [ ] **Step 2 : Poser un faux `smbclient`**

Créer `web/app/e2e/faux-smbclient.sh`, exécutable, qui rend des sorties captées :

```sh
#!/bin/sh
# Faux smbclient du parcours de bout en bout.
#
# Le parcours n'a pas de NAS. Il en simule un, avec les sorties **captées** sur
# un NAS Synology réel via samba 4.19.5 — c'est ce qui rend l'assistant réseau
# jouable de bout en bout sans matériel, sans inventer un format.
case "$*" in
  *--version*) echo "Version 4.19.5-Ubuntu" ;;
  *-L*-g*)
    echo "Disk|music|System default shared folder"
    echo "IPC|IPC\$|IPC Service ()"
    echo "SMB1 disabled -- no workgroup available"
    ;;
  *-c*ls*)
    echo "  .                                  DA        0  Fri Apr 17 14:46:30 2026"
    echo "  ..                                  D        0  Sun Aug 16 16:23:48 2026"
    echo "  Yann Tiersen                       DA        0  Tue Jul 17 23:07:00 2018"
    echo "  piste.mp3                           A  1234567  Mon Aug 11 20:12:33 2025"
    printf "\n\t\t102400 blocks of size 1024. 102380 blocks available\n"
    ;;
  *) exit 1 ;;
esac
```

Dans `serve.mjs`, préfixer le `PATH` du plugin par le répertoire qui le
contient.

- [ ] **Step 3 : Écrire le parcours**

Ajouter dans `files.spec.ts` :

```ts
test('declarer un dossier de l appareil par l assistant, puis tout ajouter', async ({ page }) => {
  await page.goto('/plugins/files')
  await page.click('[data-add-device]')
  // La popin ouvre sur les volumes : c'est tout l'objet du chantier, on ne
  // tape plus un chemin absolu qu'aucun ecran n'affiche.
  await page.click('[data-volume]')
  await expect(page.locator('[data-choix-dossier]').first()).toBeVisible()
  await page.click('[data-choisir]')
  await expect(page.locator('[data-source]')).toHaveCount(1)

  await page.click('[data-add-all]')
  await expect(page.locator('[data-track-row]').first()).toBeVisible()
})

test('declarer un partage par l assistant, sans NAS', async ({ page }) => {
  await page.goto('/plugins/files')
  await page.click('[data-add-share]')
  await page.fill('[data-host]', '192.168.1.20')
  await page.fill('[data-user]', 'steven')
  await page.fill('[data-password]', 'secret')
  await page.click('[data-connect]')
  // Le partage administratif IPC$ ne doit pas apparaitre : il ferait douter du
  // bon partage.
  await expect(page.locator('[data-share]')).toHaveCount(1)
  await page.click('[data-share]')
  await expect(page.locator('[data-choix-dossier]')).toHaveCount(1)
  await page.click('[data-choisir]')
  await expect(page.locator('[data-source]')).toHaveCount(1)
})
```

- [ ] **Step 4 : Lancer le parcours deux fois**

Run : `npx playwright test files --repeat-each=2`
Expected: PASS. Deux passages parce qu'un défaut de course ne se voit pas au
premier.

- [ ] **Step 5 : Vérification complète**

Run :
```bash
wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/ihm-sources && cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings"
npm test && npm run typecheck
```
Expected: tout PASS.

- [ ] **Step 6 : Commit**

```bash
git add web/app/e2e
git commit -m "test(e2e): les deux assistants joues de bout en bout, sans NAS ni privilege"
```

---

## Vérifications qui n'appartiennent pas à ce plan

À faire sur le Pi, hors de portée d'ici :

- Le dialecte SMB négocié avec le NAS réel, et la présence de `smbclient` et
  `cifs-utils` sur l'installation existante.
- La propagation du montage dans l'espace de noms durci de `ritornello.service`
  (recours documenté : `BindPaths=/mnt/ritornello`).
Les formats de `smbclient`, eux, **ne sont plus une inconnue** : succès comme
échecs ont été captés sur un NAS Synology réel avec le client samba 4.19.5, et
les fixtures des tests sont ces sorties telles quelles. Le filet reste en
place — `parse_ls` refuse de rendre un dossier vide quand elle ne sait pas
lire — pour le jour où un autre serveur répondra autrement.

Reste à vérifier sur le Pi que la version de `smbclient` empaquetée pour
Raspberry Pi OS produit bien les mêmes formats que celle d'Ubuntu 24.04.

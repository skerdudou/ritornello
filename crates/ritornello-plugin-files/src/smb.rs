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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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
    let enfant = tokio::process::Command::new(SMBCLIENT)
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
    if depart.starts_with('-') || depart.contains('\0') || depart.contains("..") {
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
}

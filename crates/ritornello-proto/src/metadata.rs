//! Protocole du genre `metadata` : le cœur annonce ce qui joue, un plugin
//! renvoie ce qu'il en sait.
//!
//! Contrairement à `source` (requête/réponse corrélée par `id`) et à `display`
//! ou `input` (sens unique), ce protocole est **bidirectionnel non corrélé** :
//! chaque côté émet quand il a quelque chose à dire. Le cœur ne demande rien,
//! parce qu'il n'a aucun moyen de savoir si un plugin saura répondre ni au bout
//! de combien de temps ; le plugin n'attend pas de réponse, parce qu'un
//! enrichissement n'est ni accepté ni refusé, il est simplement retenu ou
//! périmé.
//!
//! Le garde-fou contre la péremption est l'**écho de l'identité** : un
//! enrichissement porte l'identité auquel il se rapporte, et le cœur jette
//! celui qui ne correspond plus à ce qui joue. Sans cet écho, la réponse lente
//! d'un plugin sur le morceau précédent viendrait écraser le morceau courant.

use serde::{Deserialize, Serialize};

/// Ce que la Source dit de ce qu'elle joue, transporté à côté de la vue.
///
/// Trois états sont nécessaires, et c'est pourquoi ce type est un enum plutôt
/// qu'un `Option` : l'absence du champ dans une trame (« cette réponse ne dit
/// rien de l'identité ») ne doit pas être confondue avec `Nothing` (« plus rien
/// ne joue »), et serde ramène `null` et l'absence à la même valeur pour un
/// `Option`. Les trois cas sont donc : champ absent, `Playing`, `Nothing`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// `value` et non `identity` : imbriqué dans `SourceMessage.identity`, ce dernier
// donnerait `"identity":{"state":"Playing","identity":{…}}` — le protocole se
// veut lisible à l'œil dans un `journalctl`.
#[serde(tag = "state", content = "value")]
pub enum IdentityUpdate {
    /// Identité **opaque**, produite par la Source, jamais interprétée par le
    /// cœur — même principe que le JSON opaque du protocole `admin`. Le cœur
    /// se contente de comparer deux identités par égalité.
    Playing(serde_json::Value),
    /// Plus rien ne joue : le cœur oublie l'identité courante et prévient les
    /// plugins `metadata` pour qu'ils cessent leur travail.
    Nothing,
}

/// L'état partiel du morceau, tel qu'un greffon a besoin de le voir.
///
/// Un type dédié plutôt que `Morceau` : ce dernier porte `cover_href` et
/// `cover_origin`, qui sont des URL **locales de l'appareil** — elles n'ont
/// aucun sens pour un greffon et l'inviteraient à croire qu'il peut les lire.
///
/// Un champ à `None` est un champ que personne n'a encore rempli. C'est ce qui
/// permet à un greffon de ne travailler que sur ce qui manque.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Known {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    /// Une pochette est **déjà tenue**. Un booléen, jamais l'image : un greffon
    /// n'a pas besoin de la voir pour décider s'il doit en chercher une, et la
    /// transmettre alourdirait chaque trame pour rien.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cover: bool,
    // Pas de `links` ici, et ce n'est pas un oubli : `Known` existe pour qu'un
    // greffon ne travaille que sur ce qui manque, or aucun des nôtres ne
    // *cherche* des liens — il recopie ceux de la réponse qu'il lit déjà. Le
    // champ ne changerait la décision d'aucun d'eux.
    /// Ce que le **flux lui-même** a annoncé, brut : ni découpé, ni composé,
    /// ni arbitré.
    ///
    /// Pas une redite de `title`. `title` est le résultat d'un arbitrage entre
    /// plusieurs contributeurs et peut donc venir d'un greffon ; ce champ est
    /// un fait d'un seul émetteur, la station.
    ///
    /// Il existe parce que seule la forme brute peut être **redécoupée**, et
    /// qu'un greffon a besoin de la revoir même après avoir lui-même écrasé le
    /// titre composé. L'identité d'une radio est l'URL de son flux, donc elle
    /// ne change pas d'un morceau à l'autre : le garde-fou de péremption de
    /// `Metadonnees::ajoute` ne périme rien, et `set_icy` n'efface pas les
    /// enrichissements. Sans ce champ, un greffon qui corrige une fois ne
    /// reverrait plus jamais ce que la station annonce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_title: Option<String>,
}

impl Known {
    /// Vrai si personne n'a encore rien rempli.
    ///
    /// Sert de `skip_serializing_if` : une trame qui ne dit rien de ce qui est
    /// connu doit rester identique à l'octet près à ce qu'elle était avant ce
    /// chantier, et le protocole se veut lisible à l'œil dans un journalctl.
    /// `year` en fait partie, et l'oublier serait une perte silencieuse : ce
    /// prédicat est le `skip_serializing_if` de `NowPlaying::known`, donc un
    /// `Known` jugé vide **disparaît de la trame**. Une année seule connue
    /// n'atteindrait jamais les greffons.
    pub fn est_vide(&self) -> bool {
        self.artist.is_none()
            && self.title.is_none()
            && self.album.is_none()
            && self.duration_s.is_none()
            && self.year.is_none()
            && !self.cover
    }
}

/// Ce qu'un contributeur a trouvé comme pochette, à charge pour le cœur
/// d'aller la chercher.
///
/// Deux formes **explicitement distinctes** plutôt qu'une chaîne que le cœur
/// devinerait : le chemin sert au `folder.jpg` posé sur un partage, qui existe
/// déjà sur le disque — rien à extraire, aucun fichier temporaire.
///
/// Jamais des octets : le canal des greffons reste textuel, donc lisible à
/// l'œil dans un `journalctl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoverRef {
    /// URL externe, à télécharger. `https` uniquement, vers un nom d'hôte.
    Url { url: String },
    /// Chemin absolu d'un fichier image déjà présent sur le disque.
    Path { path: String },
}

/// Extensions acceptées pour un `CoverRef::Path`.
const EXTENSIONS_IMAGE: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

impl CoverRef {
    /// Normalise et **valide**. `None` = à jeter.
    ///
    /// Ces valeurs arrivent d'un autre processus et le cœur va agir dessus : il
    /// faut les traiter comme des entrées, pas comme des données de confiance.
    ///
    /// Publique, parce qu'une pochette entre dans le cœur par **deux** canaux :
    /// l'enrichissement d'un greffon (`Enrichment::cleaned`, juste en dessous)
    /// et la trame d'une Source (`SourceMessage::cover`). Privée, cette
    /// méthode ne couvrait que le premier — la couche documentée comme
    /// propriétaire de la validation de forme ne l'était donc que sur la
    /// moitié de ses entrées, et le second canal reposait entièrement sur les
    /// contrôles propres du cœur.
    pub fn validee(self) -> Option<Self> {
        match self {
            Self::Url { url } => {
                let url = url.trim();
                let reste = url.strip_prefix("https://")?;
                let hote = reste.split(['/', '?', '#']).next().unwrap_or("");
                if hote.is_empty() || hote.contains('@') {
                    return None;
                }
                // Une adresse IP littérale est refusée : un nom d'hôte, et rien
                // d'autre. `[::1]` est écarté par le crochet, `192.168.1.1` par
                // le fait que tous ses libellés sont numériques.
                let sans_port = hote.split(':').next().unwrap_or("");
                if sans_port.starts_with('[')
                    || (!sans_port.is_empty()
                        && sans_port.split('.').all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit())))
                {
                    return None;
                }
                if !sans_port.contains('.') {
                    return None;
                }
                Some(Self::Url { url: url.to_string() })
            }
            Self::Path { path } => {
                let path = path.trim();
                if !path.starts_with('/') {
                    return None;
                }
                let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
                EXTENSIONS_IMAGE.contains(&ext.as_str()).then(|| Self::Path { path: path.to_string() })
            }
        }
    }
}

/// Un lien vers la plateforme d'écoute où ce morceau se trouve.
///
/// Un enum fermé, et non un couple `(nom, url)` : c'est **la** décision de
/// sécurité de ce type. La variante nomme la plateforme, et [`Self::validee`]
/// impose alors l'hôte qui lui correspond. Une source tierce ne peut donc pas
/// faire afficher à l'IHM un lien cliquable vers un domaine de son choix — au
/// pire elle ment sur son propre domaine, ce qui est le risque qu'on accepte
/// déjà en la croyant sur le titre.
///
/// Avec un champ `plateforme: String` libre, un `{"plateforme":"deezer",
/// "url":"https://ailleurs.example/x"}` serait rendu tel quel : le contrôle
/// n'aurait plus rien à quoi se raccrocher.
///
/// Ajouter une plateforme est une modification de ce fichier, volontairement :
/// elle oblige à écrire son hôte ici, à côté des autres.
///
/// **Conséquence de l'enum interne-taggé, acceptée :** une trame nommant une
/// plateforme que ce fichier ne connaît pas ne perd pas ce lien-là, elle fait
/// échouer la désérialisation de **tout** l'enrichissement — `serde` n'a pas de
/// variante de repli à lui donner, et un `#[serde(other)]` en ajouterait une
/// qui n'aurait ni hôte admis ni icône. C'est admis parce que cœur et greffons
/// se déploient **ensemble**, d'un seul paquet : un greffon ne peut pas se
/// retrouver en avance sur le cœur qui le lit. Le jour où ce ne serait plus
/// vrai, il faudrait un `Vec<serde_json::Value>` dépouillé lien par lien, et ce
/// jour-là seulement — l'ajouter d'avance coûterait le typage qui fait toute la
/// valeur de ce type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case")]
pub enum Link {
    Youtube { url: String },
    Deezer { url: String },
    AppleMusic { url: String },
}

impl Link {
    /// Les hôtes admis pour cette plateforme, et **rien d'autre**.
    ///
    /// Une liste et non un hôte unique, parce qu'une même plateforme se publie
    /// sous plusieurs noms et que ce sont bien ses liens : `youtu.be` est la
    /// forme raccourcie que YouTube émet lui-même, `music.youtube.com` sa
    /// déclinaison musicale. Ils doivent donc marcher, et **avec la même
    /// icône** — ce qui vient gratuitement, puisque c'est la variante et non
    /// l'hôte qui choisit l'icône côté IHM.
    ///
    /// Radio France n'émet aujourd'hui que du `www.youtube.com` (mesuré le
    /// 2026-08-27) ; les autres formes sont admises d'avance plutôt qu'après
    /// une panne silencieuse le jour où le tiers change d'avis.
    ///
    /// **Cette liste est la frontière de sécurité du type.** Y ajouter un nom
    /// est une décision, pas une formalité : tout ce qui y figure devient un
    /// lien que l'appareil rendra cliquable sur la foi d'un tiers.
    fn hotes_admis(&self) -> &'static [&'static str] {
        match self {
            Self::Youtube { .. } => {
                &["www.youtube.com", "youtube.com", "m.youtube.com", "music.youtube.com", "youtu.be"]
            }
            Self::Deezer { .. } => &["www.deezer.com", "deezer.com"],
            Self::AppleMusic { .. } => &["music.apple.com"],
        }
    }

    fn url(&self) -> &str {
        match self {
            Self::Youtube { url } | Self::Deezer { url } | Self::AppleMusic { url } => url,
        }
    }

    /// Normalise et **valide**. `None` = à jeter.
    ///
    /// La comparaison porte sur l'**autorité** et non sur un préfixe de chaîne :
    /// `https://www.deezer.com.evil.example/x` a bien le vrai domaine en
    /// préfixe sans en être un. C'est la même erreur que le greffon OUI FM a
    /// documentée pour son hôte d'images, et elle se referme ici pour tout le
    /// monde d'un coup.
    ///
    /// Le port est refusé, et l'info utilisateur (`@`) aussi : `https://
    /// www.deezer.com@evil.example/` a pour hôte réel `evil.example`.
    pub fn validee(self) -> Option<Self> {
        let admis = self.hotes_admis();
        let url = self.url().trim().to_string();
        let reste = url.strip_prefix("https://")?;
        let autorite = reste.split(['/', '?', '#']).next().unwrap_or("");
        // Égalité stricte contre chaque nom admis, jamais un suffixe :
        // `evil-youtube.com` et `youtube.com.evil.example` échouent tous deux,
        // là où un `ends_with` laisserait passer le premier et un `starts_with`
        // le second.
        if !admis.contains(&autorite) {
            return None;
        }
        Some(match self {
            Self::Youtube { .. } => Self::Youtube { url },
            Self::Deezer { .. } => Self::Deezer { url },
            Self::AppleMusic { .. } => Self::AppleMusic { url },
        })
    }
}

/// Borne de vraisemblance d'une année, des deux côtés.
///
/// Ces valeurs viennent d'un tiers ou d'une étiquette de fichier arbitraire.
/// Une année à 0 ou à 90210 n'apprend rien et enlaidit l'écran ; la refuser ne
/// coûte que ce qu'elle valait.
const ANNEE_MIN: u16 = 1000;
const ANNEE_MAX: u16 = 2999;

/// Lit une année dans les formes que rendent nos sources. `None` = à jeter.
///
/// Trois formes mesurées, d'où l'existence de cette fonction plutôt qu'un
/// `parse()` chez chaque appelant : MusicBrainz rend `"1987"` ou
/// `"2017-06-23"`, la grille Radio France rend le **nombre** 1952, et les
/// étiquettes de fichiers rendent un peu tout, `"1972-00-00"` compris.
///
/// La règle porte sur la **longueur** de la tête numérique, et non sur sa
/// valeur : 4 chiffres, c'est l'année ; 8 chiffres, c'est un `YYYYMMDD`
/// compact (que les étiquettes ID3 écrivent, `TDRC` autorisant la forme
/// resserrée) dont on garde les quatre premiers ; toute autre longueur est
/// jetée. Sans cette règle, `"19590817"` sortait `None` (hors bornes) et
/// surtout `"90210"` sortait 9021 — la borne haute ne rattrape pas un code
/// postal, elle ne fait que tronquer le nombre qu'il forme.
pub fn annee_valide(brut: &str) -> Option<u16> {
    let tete: String = brut.trim().chars().take_while(char::is_ascii_digit).collect();
    let tete = match tete.len() {
        4 => tete.as_str(),
        8 => &tete[..4],
        _ => return None,
    };
    let annee = tete.parse::<u16>().ok()?;
    (ANNEE_MIN..=ANNEE_MAX).contains(&annee).then_some(annee)
}

/// Cœur → plugin. Émis à chaque changement de ce qui joue, et à l'arrêt
/// (`identity: None`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NowPlaying {
    /// Nom de la Source active (`"radio"`, `"cd"`…), pour qu'un plugin puisse
    /// se taire d'emblée sur une source qu'il ne traite pas, sans avoir à
    /// inspecter la forme de l'identité.
    pub source: String,
    /// `None` = plus rien ne joue.
    #[serde(default)]
    pub identity: Option<serde_json::Value>,
    /// Ce qui est **déjà connu** du morceau, tous étages confondus.
    ///
    /// `#[serde(default)]` : une trame écrite par un binaire antérieur se
    /// relit, et un greffon qui ignore le champ fonctionne exactement comme
    /// avant — c'est ce qui rend la refonte déployable greffon par greffon.
    /// `skip_serializing_if` : tant que rien n'est connu, la trame reste
    /// identique à l'octet près à ce qu'elle était avant ce champ.
    #[serde(default, skip_serializing_if = "Known::est_vide")]
    pub known: Known,
}

/// Plugin → cœur. Émis quand le plugin apprend quelque chose.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Enrichment {
    /// **Écho** de l'identité concernée : le garde-fou de péremption.
    pub identity: serde_json::Value,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub duration_s: Option<u32>,
    /// Année de sortie. Validée par [`annee_valide`] chez le contributeur, et
    /// rebornée ici par [`Self::cleaned`] : la valeur arrive d'un autre
    /// processus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    /// Les plateformes d'écoute où ce morceau se trouve.
    ///
    /// Une liste et non une `Option` : un contributeur peut en connaître
    /// plusieurs d'un coup (OUI FM rend Deezer **et** Apple Music dans la même
    /// trame), et la liste vide dit déjà « aucune ».
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// Écoulé dans le morceau **au moment de l'émission**, en secondes.
    ///
    /// Un écoulé relatif plutôt qu'un horodatage absolu : rien à synchroniser
    /// entre deux horloges, et c'est la convention de `duration_s` juste
    /// au-dessus. Le cœur l'ancre à la réception et l'avance lui-même ensuite
    /// (voir `Core::rafraichit_position`).
    #[serde(default)]
    pub position_s: Option<u32>,
    /// La pochette que ce contributeur a trouvée.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverRef>,
    /// Ce contributeur ne fait que **compléter** : il ne remplace aucun champ
    /// déjà renseigné.
    ///
    /// Défaut `false` = il écrase, ce qui est la règle actuelle du projet (« a
    /// plugin takes precedence over ICY and over file tags under all
    /// circumstances ») et ce qui évite de toucher aux greffons livrés.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fill_only: bool,
}

impl Enrichment {
    /// Ramène à `None` toute chaîne vide ou blanche.
    ///
    /// Un plugin qui ne connaît pas l'artiste peut aussi bien envoyer `null`
    /// qu'une chaîne vide — les deux disent la même chose. Normaliser ici évite
    /// que le reste du cœur ait à traiter deux cas, et surtout évite qu'un
    /// `title: ""` compte comme une réponse et bloque un plugin moins
    /// prioritaire qui, lui, connaît le titre (voir `is_empty`).
    pub fn cleaned(mut self) -> Self {
        fn vide(champ: &mut Option<String>) {
            if champ.as_deref().is_some_and(|s| s.trim().is_empty()) {
                *champ = None;
            } else if let Some(s) = champ {
                *s = s.trim().to_string();
            }
        }
        vide(&mut self.artist);
        vide(&mut self.title);
        vide(&mut self.album);
        self.cover = self.cover.take().and_then(CoverRef::validee);
        // Rebornée ici même si le contributeur est censé l'avoir fait : cette
        // valeur traverse un socket, et cette couche est celle qui est
        // documentée comme propriétaire de la validation de forme.
        self.year = self.year.filter(|a| (ANNEE_MIN..=ANNEE_MAX).contains(a));
        // Chaque lien passe par sa propre validation d'hôte ; ceux qui la
        // ratent sont **jetés un à un**, pas la liste entière : un `deezerId`
        // douteux ne doit pas faire perdre le lien YouTube qui l'accompagne.
        self.links = self.links.drain(..).filter_map(Link::validee).collect();
        self
    }

    /// Vrai si l'enrichissement n'apporte aucune information.
    ///
    /// À appeler **après** `cleaned`. Un tel enrichissement compte comme une
    /// non-réponse dans l'arbitrage : un plugin qui reconnaît l'identité mais
    /// n'a encore rien appris ne doit pas bloquer un plugin moins prioritaire.
    pub fn is_empty(&self) -> bool {
        self.artist.is_none() && self.title.is_none() && self.album.is_none()
    }
}

/// Ce qui est affichable du morceau en cours.
///
/// `origin` dit **qui** a fourni l'information (`"icy"` ou le nom du plugin
/// gagnant) : sans elle, un affichage douteux ne serait attribuable à personne,
/// et c'est exactement la question qu'on se pose devant un titre faux.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Morceau {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub duration_s: Option<u32>,
    /// Année de sortie, quand un contributeur la connaît.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    /// Les plateformes d'écoute, déjà validées (voir [`Link::validee`]).
    ///
    /// Voyage dans la charge utile commune plutôt que par un canal réservé à
    /// l'IHM : c'est la convention du projet, chaque afficheur compose ce
    /// qu'il sait montrer. Un afficheur texte l'ignore, l'IHM web en fait des
    /// boutons.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    pub origin: Option<String>,
    /// URL **locale** de la pochette, à mettre telle quelle dans un `src`.
    /// Toujours de la forme `/api/cover/{clé}` : l'IHM ne contacte jamais
    /// l'extérieur.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_href: Option<String>,
    /// Qui a fourni cette pochette : le nom de la Source, `"tags"`, ou le nom
    /// du greffon. Une seconde origine, parce que le texte et l'image peuvent
    /// venir de deux contributeurs différents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_origin: Option<String>,
}

/// A transient overlay the appliance is showing right now, carrying **both**
/// the raw value and the resolved words: a display can draw a volume gauge
/// from `level`, or simply print `text`, without needing a catalogue of its
/// own.
///
/// `remaining_ms` is informative. The core alone owns the deadline — it
/// publishes a frame when the overlay expires — so a display may animate a
/// countdown but never decides when the overlay ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Overlay {
    /// Volume/mute overlay.
    Volume { level: u8, muted: bool, text: String, remaining_ms: u32 },
    /// Pending tens offset being composed on the remote (`+10`, `+20`).
    Tens { offset: u8, text: String, remaining_ms: u32 },
    /// Ephemeral message from a source ("empty preset").
    Message { text: String, remaining_ms: u32 },
}

/// Égalité **volontairement écrite à la main** : elle ignore `remaining_ms`.
///
/// Deux incrustations qui ne diffèrent que par le temps restant décrivent le
/// même écran, et `Core::publie_etat` déduplique les trames par égalité. Une
/// dérive automatique ferait passer chaque rafraîchissement redondant pour un
/// changement — plusieurs chemins du cœur rafraîchissent pour un même
/// événement — et chaque afficheur réimprimerait la même chose.
///
/// Écrite ici, sur `Overlay`, et non sur `PlayerState` : au niveau de la
/// charge utile il faudrait comparer à la main tous les autres champs pour ne
/// traiter spécialement qu'un champ imbriqué dans un enum sous une `Option`,
/// et chaque champ ajouté plus tard serait un oubli en puissance.
impl PartialEq for Overlay {
    fn eq(&self, autre: &Self) -> bool {
        match (self, autre) {
            (
                Self::Volume { level: a, muted: ma, text: ta, .. },
                Self::Volume { level: b, muted: mb, text: tb, .. },
            ) => a == b && ma == mb && ta == tb,
            (Self::Tens { offset: a, text: ta, .. }, Self::Tens { offset: b, text: tb, .. }) => {
                a == b && ta == tb
            }
            (Self::Message { text: ta, .. }, Self::Message { text: tb, .. }) => ta == tb,
            _ => false,
        }
    }
}

impl Overlay {
    /// Replaces the remaining time, computed at publication from the deadline
    /// the core holds. The `remaining_ms` stored in `self` is therefore never
    /// read — and since equality ignores it, refreshing it does not defeat the
    /// frame deduplication.
    #[must_use]
    pub fn avec_restant(self, restant_ms: u32) -> Self {
        match self {
            Self::Volume { level, muted, text, .. } => {
                Self::Volume { level, muted, text, remaining_ms: restant_ms }
            }
            Self::Tens { offset, text, .. } => Self::Tens { offset, text, remaining_ms: restant_ms },
            Self::Message { text, .. } => Self::Message { text, remaining_ms: restant_ms },
        }
    }
}

/// Ce que fait le lecteur, en un mot. `Stopped` par défaut : ne rien savoir,
/// c'est ne rien jouer — la même convention que `can_eject`, où l'absence
/// d'information vaut l'absence de capacité.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Playback {
    #[default]
    Stopped,
    Playing,
    Paused,
}

impl Playback {
    /// Sert le `skip_serializing_if` du champ : la valeur par défaut ne
    /// voyage pas, donc les trames existantes restent identiques à l'octet.
    /// Une méthode et non une fermeture : `skip_serializing_if` exige un
    /// chemin de fonction.
    pub fn is_stopped(&self) -> bool {
        matches!(self, Playback::Stopped)
    }
}

/// État du lecteur diffusé à la SPA : ce qui est volatil, et qui a donc besoin
/// d'être **poussé**.
///
/// Un seul état et un seul canal pour tout ce qui bouge — source active, volume,
/// muet, veille, et le morceau quand on le connaît. La route `/api/status`, elle,
/// porte le contrat de navigation (quels plugins existent, lesquels ont une page
/// d'admin) : structurellement stable, lue une fois au montage. Y mêler du
/// volatil obligerait la SPA à la resonder en boucle pour afficher un volume.
///
/// Le morceau est **aplati** dans le JSON (`serde(flatten)`) : l'IHM reçoit un
/// objet plat, sans avoir à distinguer deux niveaux pour un même encart.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    /// Nom de la Source active, pour que la SPA sache de quoi elle parle.
    pub source: String,
    pub volume: u8,
    pub muted: bool,
    pub standby: bool,
    /// Touche numérotée correspondant à ce qui joue, telle que la Source active l'a
    /// déclarée (présélection radio, piste cd) : c'est ce que la télécommande
    /// de l'IHM met en évidence. `None` = rien ne joue, ou la Source n'a rien
    /// déclaré.
    pub preset: Option<u8>,
    /// Nombre de présélections numérotées offertes par la Source active
    /// (stations pour la radio, pistes pour le cd), tel qu'elle l'a déclaré.
    /// `None` = rien déclaré : l'IHM retombe sur la grille 1-9 historique.
    /// `Some(0)` = rien à numéroter (cd sans disque) : aucune touche.
    pub preset_count: Option<u8>,
    /// Nom lisible de la présélection donnée par `preset`, tel que la Source
    /// active l'a déclaré (le nom configuré de la station pour la radio).
    /// `None` : la Source ne nomme rien à cet emplacement (le cd, dont
    /// « audio CD » n'a rien à voir avec une présélection nommée), ou rien ne
    /// joue. Vit et meurt avec `preset` — voir `Core::set_identity`.
    pub preset_name: Option<String>,
    /// The appliance's current state as a **resolved sentence**: the status a
    /// source declared ("NO DISC", "AUDIO CD") or the core's standby word.
    /// One slot, because there is never more than one status at a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The transient overlay showing right now, if any. Displays render it as
    /// they see fit; the SPA ignores it (it shows the volume in plain sight
    /// and has its own toasts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<Overlay>,
    /// Où en est ce qui joue, en secondes, **à l'instant de la publication**.
    ///
    /// `None` = personne n'a de quoi répondre : rien ne joue, ou c'est un flux
    /// que nul plugin `metadata` ne suit. Deux fournisseurs alimentent ce
    /// champ sans jamais se disputer — mpv pour un contenu fini, un plugin
    /// `metadata` pour un flux — parce que le contexte décide lequel des deux
    /// a le droit de parler (voir `Core::rafraichit_position`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_s: Option<u32>,
    /// Ce que fait le lecteur. Additif, à l'idiome de `InputMessage.held` et
    /// de `PluginStatus.stalled` : absent du JSON quand il vaut `Stopped`,
    /// donc aucune trame existante ne change et une trame ancienne se relit.
    ///
    /// Distinct de `position_s.is_some()` : une lecture en pause garde sa
    /// position, et un flux qui joue peut n'en avoir aucune.
    #[serde(default, skip_serializing_if = "Playback::is_stopped")]
    pub playback: Playback,
    /// Ce qui joue accepte un déplacement : c'est le `finite` que la Source a
    /// déclaré à son `Play`, rendu visible aux consommateurs.
    ///
    /// Un champ à part entière plutôt qu'une déduction de `duration_s` : les
    /// deux notions divergent exactement là où ça compte — Radio France
    /// annonce la durée d'un morceau sur un direct qu'on ne peut pas
    /// rembobiner, un fichier sans étiquette de durée reste parcourable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seekable: bool,
    /// La Source active a de quoi éjecter (voir `SourceMessage::can_eject`) :
    /// c'est ce qui permet à la télécommande web de griser sa touche Eject
    /// plutôt que d'émettre une commande que la Source jettera en silence.
    ///
    /// **Faux par défaut** : ne pas savoir, c'est n'offrir rien — la même
    /// convention que les capacités d'extinction de `system.rs`. Un booléen et
    /// non un `Option` : côté consommateur, « la Source n'a rien déclaré » et
    /// « la Source ne peut pas éjecter » appellent le même bouton grisé, et un
    /// troisième état n'aurait aucun rendu propre.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub can_eject: bool,
    /// Comment cet appareil écrit une heure et une date, tel que son
    /// propriétaire l'a réglé.
    ///
    /// **Une préférence de rendu dans la trame d'état, et il faut dire
    /// pourquoi.** Un afficheur ne doit jamais aller chercher quoi que ce soit
    /// de côté — tout ce qu'il montre arrive par ce canal — et l'horloge qu'il
    /// dessine en veille est justement quelque chose qu'il montre. La
    /// solution inverse (le cœur pousse l'heure **déjà écrite**) a été écartée :
    /// elle imposerait une trame par minute, pour toujours, y compris quand
    /// personne ne regarde. Ici la valeur ne bouge qu'au geste de l'utilisateur.
    ///
    /// Additif, à l'idiome du reste de la structure : absent du JSON à sa
    /// valeur par défaut, donc aucune trame existante ne change de forme.
    #[serde(default, skip_serializing_if = "Horloge::est_defaut")]
    pub clock: Horloge,
    #[serde(flatten)]
    pub morceau: Morceau,
}

/// Les deux réglages d'écriture du temps, tels qu'ils voyagent aux afficheurs.
///
/// Deux champs séparés parce que ce sont deux choix indépendants : l'ordre des
/// composants d'une date et le format 12/24 h ne varient pas ensemble d'un pays
/// à l'autre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Horloge {
    /// L'ordre des composants d'une date.
    #[serde(default)]
    pub date: FormatDate,
    /// Heure sur 24 h plutôt que sur 12 h.
    ///
    /// **Le défaut est 24 h**, et le champ s'écrit donc « sur 12 h » pour que
    /// la valeur par défaut soit `false` et disparaisse du JSON — la même
    /// mécanique additive que `playback` ou `can_eject`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub douze_heures: bool,
}

impl Horloge {
    /// Vrai pour la valeur par défaut : sert au `skip_serializing_if` de
    /// `PlayerState::clock`.
    pub fn est_defaut(&self) -> bool {
        *self == Self::default()
    }
}

/// L'ordre des composants d'une date. Miroir de `state::DateFormat` côté cœur,
/// que le protocole ne peut pas importer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatDate {
    /// `31/12/2026`
    #[default]
    DayMonthYear,
    /// `2026-12-31`
    YearMonthDay,
    /// `12/31/2026`
    MonthDayYear,
}

impl Morceau {
    /// Vrai si rien n'est connu du morceau.
    ///
    /// N'a d'appelant que dans les tests, et c'est voulu : côté IHM, c'est la
    /// SPA qui décide quoi montrer d'un état partiel, et le cœur n'a aucune
    /// raison de trancher pour elle.
    ///
    /// Cette convention n'est plus tenue par le compilateur. Elle l'était par un
    /// `#[cfg(test)]`, du temps où la structure vivait dans le cœur avec ses
    /// tests ; un tel attribut ne survit pas au passage dans un crate séparé,
    /// où il ne s'applique qu'à la compilation de ce crate-là et ferait
    /// disparaître la méthode pour tous les autres.
    pub fn est_vide(&self) -> bool {
        self.artist.is_none() && self.title.is_none() && self.album.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn now_playing_roundtrip_avec_identite() {
        let np = NowPlaying {
            source: "radio".into(),
            identity: Some(json!({"kind": "stream", "url": "https://ouifm/ouifm-high.mp3"})),
            known: Known::default(),
        };
        let back: NowPlaying = serde_json::from_str(&serde_json::to_string(&np).unwrap()).unwrap();
        assert_eq!(back, np);
    }

    #[test]
    fn now_playing_roundtrip_sans_identite() {
        let np = NowPlaying { source: "cd".into(), identity: None, known: Known::default() };
        let json = serde_json::to_string(&np).unwrap();
        let back: NowPlaying = serde_json::from_str(&json).unwrap();
        assert_eq!(back.identity, None);
    }

    #[test]
    fn enrichment_roundtrip() {
        let e = Enrichment {
            identity: json!({"kind": "disc", "track": 3}),
            artist: Some("Miles Davis".into()),
            title: Some("So What".into()),
            album: Some("Kind of Blue".into()),
            duration_s: Some(545),
            // Valeurs non-defaut : ce test verifie un aller-retour complet, et
            // un champ laisse a sa valeur par defaut ne prouverait rien de son
            // encodage. Les deux liens couvrent les deux formes du `Vec`.
            year: Some(1959),
            links: vec![
                Link::Youtube { url: "https://www.youtube.com/watch?v=zqNTltOGh5c".into() },
                Link::Deezer { url: "https://www.deezer.com/track/9956167".into() },
            ],
            position_s: None,
            cover: None,
            fill_only: false,
        };
        let back: Enrichment = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn enrichment_accepte_les_champs_absents() {
        // Un plugin minimal n'envoie que ce qu'il connaît : les champs manquants
        // ne doivent pas faire échouer la lecture de la trame.
        let e: Enrichment = serde_json::from_str(r#"{"identity":{"k":1},"title":"Bikwix"}"#).unwrap();
        assert_eq!(e.title.as_deref(), Some("Bikwix"));
        assert_eq!(e.artist, None);
        assert_eq!(e.duration_s, None);
    }

    #[test]
    fn identity_update_distingue_les_trois_etats() {
        // Playing et Nothing doivent se distinguer sur le fil, et l'absence du
        // champ (testée dans source.rs) est un troisième cas.
        let joue = IdentityUpdate::Playing(json!({"kind": "stream"}));
        assert_eq!(
            serde_json::to_string(&joue).unwrap(),
            r#"{"state":"Playing","value":{"kind":"stream"}}"#
        );
        assert_eq!(serde_json::to_string(&IdentityUpdate::Nothing).unwrap(), r#"{"state":"Nothing"}"#);
        let back: IdentityUpdate = serde_json::from_str(r#"{"state":"Nothing"}"#).unwrap();
        assert_eq!(back, IdentityUpdate::Nothing);
    }

    #[test]
    fn un_lien_ne_peut_viser_que_sa_propre_plateforme() {
        // LA propriete de securite de ce type. Ces URL viennent d'un tiers, et
        // l'IHM en fait un lien cliquable : sans ce controle, une trame
        // hostile place un lien vers la cible de son choix sous une icone de
        // confiance.
        for mauvaise in [
            // L'hote d'une autre plateforme, ou un hote quelconque.
            "https://www.deezer.com/track/1",
            "https://evil.example/x",
            // Le vrai domaine en simple prefixe de chaine, et en simple
            // suffixe : les deux erreurs classiques d'un controle par
            // `starts_with` ou `ends_with`.
            "https://www.youtube.com.evil.example/x",
            "https://evil-youtube.com/x",
            // Confusion userinfo : l'hote reel est evil.example.
            "https://www.youtube.com@evil.example/x",
            // Schema.
            "http://www.youtube.com/watch?v=a",
            "javascript:alert(1)",
            "",
        ] {
            assert!(
                Link::Youtube { url: mauvaise.into() }.validee().is_none(),
                "accepte a tort : {mauvaise:?}"
            );
        }
    }

    #[test]
    fn les_formes_courtes_de_youtube_sont_admises_et_gardent_la_meme_icone() {
        // Decision du proprietaire : `youtu.be` doit marcher comme
        // `youtube.com`, avec la meme icone. C'est gratuit — c'est la variante
        // et non l'hote qui choisit l'icone cote IHM — a condition que la
        // validation admette la forme courte, ce que ce test verrouille.
        for bonne in [
            "https://www.youtube.com/watch?v=zIqlKJj9IlY",
            "https://youtube.com/watch?v=a",
            "https://m.youtube.com/watch?v=a",
            "https://music.youtube.com/watch?v=a",
            "https://youtu.be/zIqlKJj9IlY",
        ] {
            let l = Link::Youtube { url: bonne.into() }.validee();
            assert!(matches!(l, Some(Link::Youtube { .. })), "refuse a tort : {bonne:?}");
        }
        // Et les deux autres plateformes gardent leurs propres hotes.
        assert!(Link::Deezer { url: "https://www.deezer.com/track/1".into() }.validee().is_some());
        assert!(Link::Deezer { url: "https://deezer.com/track/1".into() }.validee().is_some());
        assert!(Link::AppleMusic { url: "https://music.apple.com/us/song/1".into() }.validee().is_some());
        // `youtu.be` n'ouvre pas la porte aux autres variantes.
        assert!(Link::Deezer { url: "https://youtu.be/a".into() }.validee().is_none());
    }

    #[test]
    fn un_lien_invalide_ne_fait_pas_perdre_les_autres() {
        // Un identifiant douteux chez un fournisseur ne doit pas couter le lien
        // valide qui l'accompagne dans la meme trame.
        let e = Enrichment {
            identity: json!(1),
            links: vec![
                Link::Deezer { url: "https://evil.example/x".into() },
                Link::AppleMusic { url: "https://music.apple.com/us/song/1443171670".into() },
            ],
            ..Default::default()
        }
        .cleaned();
        assert_eq!(
            e.links,
            vec![Link::AppleMusic { url: "https://music.apple.com/us/song/1443171670".into() }]
        );
    }

    #[test]
    fn une_annee_hors_bornes_est_refusee() {
        // Ces valeurs viennent d'etiquettes de fichiers arbitraires et de
        // tiers. Une annee a 0 ou a 90210 n'apprend rien et enlaidit l'ecran.
        for brut in ["0", "999", "3000", "90210", "195", "", "abc", "-1959"] {
            assert_eq!(annee_valide(brut), None, "accepte a tort : {brut:?}");
        }
        // Les trois formes mesurees chez nos sources.
        assert_eq!(annee_valide("1987"), Some(1987), "MusicBrainz, annee seule");
        assert_eq!(annee_valide("2017-06-23"), Some(2017), "MusicBrainz, date complete");
        assert_eq!(annee_valide("1972-00-00"), Some(1972), "etiquette de fichier bancale");
        assert_eq!(annee_valide("  1959  "), Some(1959), "elague");
        // La forme compacte des etiquettes ID3 (`TDRC` autorise `YYYYMMDD`) :
        // la tete numerique fait alors 8 chiffres d'affilee, et sans regle sur
        // la longueur elle sortait `None` faute de tenir dans un `u16`.
        assert_eq!(annee_valide("19590817"), Some(1959), "etiquette ID3 compacte");
        // Le piege de la troncature naive : garder « les quatre premiers
        // chiffres » sans regarder la longueur ferait de ce code postal
        // l'annee 9021, et la borne haute ne rattraperait rien.
        assert_eq!(annee_valide("90210"), None, "un code postal n'est pas une annee");
        // Le rebornage passe aussi par `cleaned`, la couche proprietaire de la
        // validation de forme.
        let e = Enrichment { identity: json!(1), year: Some(90), ..Default::default() }.cleaned();
        assert_eq!(e.year, None);
    }

    #[test]
    fn cleaned_ramene_le_blanc_a_none_et_elague() {
        let e = Enrichment {
            identity: json!(1),
            artist: Some("   ".into()),
            title: Some("  So What  ".into()),
            album: Some(String::new()),
            duration_s: None,
            year: None,
            links: Vec::new(),
            position_s: None,
            cover: None,
            fill_only: false,
        }
        .cleaned();
        assert_eq!(e.artist, None);
        assert_eq!(e.title.as_deref(), Some("So What"));
        assert_eq!(e.album, None);
    }

    #[test]
    fn is_empty_ne_compte_que_les_champs_de_texte() {
        assert!(Enrichment { identity: json!(1), ..Default::default() }.is_empty());
        // Une durée seule ne fait pas un enrichissement affichable : elle ne
        // suffit pas à gagner l'arbitrage contre un plugin qui connaît le titre.
        let duree_seule = Enrichment { identity: json!(1), duration_s: Some(210), ..Default::default() };
        assert!(duree_seule.is_empty());
        let artiste_seul =
            Enrichment { identity: json!(1), artist: Some("FIP".into()), ..Default::default() };
        assert!(!artiste_seul.is_empty());
    }

    #[test]
    fn overlay_volume_fait_un_aller_retour_json() {
        let o = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
        let json = serde_json::to_string(&o).unwrap();
        // Étiquetage interne : un objet plat, plus simple à lire côté web qu'un
        // couple {"kind":…,"data":{…}}.
        assert!(json.contains("\"kind\":\"volume\""));
        assert!(json.contains("\"level\":65"));
        let back: Overlay = serde_json::from_str(&json).unwrap();
        assert_eq!(back, o);
    }

    #[test]
    fn overlay_cumul_et_message_font_un_aller_retour_json() {
        let t = Overlay::Tens { offset: 20, text: "PRESELECTION +20".into(), remaining_ms: 3000 };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"kind\":\"tens\""));
        assert_eq!(serde_json::from_str::<Overlay>(&json).unwrap(), t);

        let m = Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 5000 };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"kind\":\"message\""));
        assert_eq!(serde_json::from_str::<Overlay>(&json).unwrap(), m);
    }

    #[test]
    fn deux_incrustations_ne_differant_que_par_le_temps_restant_sont_egales() {
        // La garantie qui protège la déduplication de `publie_etat` : deux trames
        // qui ne diffèrent que par le temps restant décrivent le même écran. Sans
        // cette égalité, chaque rafraîchissement redondant serait poussé, et
        // chaque afficheur réimprimerait la même chose.
        let a = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
        let b = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 120 };
        assert_eq!(a, b);
    }

    #[test]
    fn une_incrustation_qui_differe_ailleurs_reste_differente() {
        // Garde-fou de l'égalité ci-dessus : elle ignore le temps restant, et rien
        // d'autre.
        let a = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
        let b = Overlay::Volume { level: 66, muted: false, text: "VOLUME 66 %".into(), remaining_ms: 4200 };
        assert_ne!(a, b);
        let c = Overlay::Message { text: "X".into(), remaining_ms: 1 };
        let d = Overlay::Message { text: "Y".into(), remaining_ms: 1 };
        assert_ne!(c, d);
    }

    #[test]
    fn avec_restant_ne_touche_qu_au_temps_restant_des_trois_variantes() {
        // La méthode n'aura son premier appelant qu'au moment où le cœur
        // publiera un temps restant frais. Sans ce test, une permutation de
        // champs entre variantes — reconstruire un `offset` depuis un `level` —
        // compilerait et ne se verrait qu'à l'intégration. L'égalité d'`Overlay`
        // ignorant `remaining_ms`, elle ne peut pas servir ici : on déstructure.
        let v = Overlay::Volume { level: 65, muted: true, text: "VOLUME MUET".into(), remaining_ms: 4000 };
        match v.avec_restant(7) {
            Overlay::Volume { level, muted, text, remaining_ms } => {
                assert_eq!((level, muted, text.as_str(), remaining_ms), (65, true, "VOLUME MUET", 7));
            }
            autre => panic!("la variante doit être préservée, obtenu {autre:?}"),
        }
        let t = Overlay::Tens { offset: 20, text: "+20".into(), remaining_ms: 4000 };
        match t.avec_restant(8) {
            Overlay::Tens { offset, text, remaining_ms } => {
                assert_eq!((offset, text.as_str(), remaining_ms), (20, "+20", 8));
            }
            autre => panic!("la variante doit être préservée, obtenu {autre:?}"),
        }
        let m = Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 4000 };
        match m.avec_restant(9) {
            Overlay::Message { text, remaining_ms } => {
                assert_eq!((text.as_str(), remaining_ms), ("PRESELECTION VIDE", 9));
            }
            autre => panic!("la variante doit être préservée, obtenu {autre:?}"),
        }
    }

    #[test]
    fn les_deux_champs_neufs_sont_absents_du_json_quand_ils_sont_vides() {
        // La charge utile de la SPA ne doit pas se remplir de nulls.
        let json = serde_json::to_string(&PlayerState::default()).unwrap();
        assert!(!json.contains("status"));
        assert!(!json.contains("overlay"));
    }

    #[test]
    fn playerstate_desyrialise_le_morceau_aplati_et_une_incrustation() {
        // C'est le chemin réel des afficheurs : `run_display_plugin` lit une
        // `DisplayFrame`, dont le `data` d'une trame d'état est **exactement**
        // cette forme (étiquetage adjacent, voir `display.rs`) — ce test reste
        // donc bien celui du contenu qui traverse le socket. `#[serde(flatten)]`
        // sur le morceau combiné à un enum étiqueté en interne (`Overlay`,
        // `kind`) est la conjonction la plus susceptible de surprendre avec
        // serde. Les autres tests de ce fichier ne couvrent que l'un ou l'autre
        // séparément ; en cas de régression ici, le symptôme serait muet côté
        // utilisateur (un `warn!` dans les logs et un écran figé).
        let json = r#"{
            "source": "radio",
            "volume": 65,
            "muted": false,
            "standby": false,
            "preset": 3,
            "preset_count": 12,
            "preset_name": "France Inter",
            "status": "RADIO",
            "overlay": {"kind": "volume", "level": 65, "muted": false, "text": "VOLUME 65 %", "remaining_ms": 4000},
            "artist": "Miles Davis",
            "title": "So What",
            "album": "Kind of Blue",
            "duration_s": 545,
            "origin": "icy"
        }"#;
        let etat: PlayerState = serde_json::from_str(json).unwrap();
        assert_eq!(etat.source, "radio");
        assert_eq!(etat.preset_name.as_deref(), Some("France Inter"));
        assert_eq!(
            etat.overlay,
            Some(Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4000 })
        );
        // Le morceau aplati : ces champs viennent du même niveau JSON que
        // `source`/`preset`/`overlay`, pas d'un objet imbriqué.
        assert_eq!(etat.morceau.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(etat.morceau.title.as_deref(), Some("So What"));
        assert_eq!(etat.morceau.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(etat.morceau.duration_s, Some(545));
        assert_eq!(etat.morceau.origin.as_deref(), Some("icy"));
    }

    #[test]
    fn player_state_serialise_position_et_seekable_quand_ils_disent_quelque_chose() {
        let etat = PlayerState {
            source: "cd".into(),
            position_s: Some(87),
            seekable: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(json.contains(r#""position_s":87"#), "{json}");
        assert!(json.contains(r#""seekable":true"#), "{json}");
    }

    /// Additif : une trame muette sur ces deux champs reste identique à
    /// l'octet près à ce qu'elle était avant ce chantier, et une trame
    /// écrite par un binaire antérieur se relit sans eux.
    #[test]
    fn player_state_tait_position_et_seekable_quand_ils_ne_disent_rien() {
        let etat = PlayerState { source: "radio".into(), ..Default::default() };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(!json.contains("position_s"), "{json}");
        assert!(!json.contains("seekable"), "{json}");
        let ancienne = r#"{"source":"radio","volume":50,"muted":false,"standby":false,"preset":null,"preset_count":null,"preset_name":null}"#;
        let relue: PlayerState = serde_json::from_str(ancienne).unwrap();
        assert_eq!(relue.position_s, None);
        assert!(!relue.seekable);
    }

    #[test]
    fn player_state_serialise_lannee_et_les_liens_quand_ils_disent_quelque_chose() {
        let etat = PlayerState {
            source: "cd".into(),
            morceau: Morceau {
                year: Some(1959),
                links: vec![Link::Youtube { url: "https://www.youtube.com/watch?v=a".into() }],
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(json.contains(r#""year":1959"#), "{json}");
        // Le `Link` est interne-taggé : la plateforme est une clé de l'objet,
        // pas un objet imbriqué de plus.
        assert!(
            json.contains(r#""links":[{"platform":"youtube","url":"https://www.youtube.com/watch?v=a"}]"#),
            "{json}"
        );
    }

    /// Additif : une trame muette sur ces deux champs reste identique à
    /// l'octet près à ce qu'elle était avant ce chantier — ni `"year":null`
    /// ni `"links":[]` — et une trame écrite par un binaire antérieur se
    /// relit sans eux.
    #[test]
    fn player_state_tait_lannee_et_les_liens_quand_ils_ne_disent_rien() {
        let etat = PlayerState { source: "radio".into(), ..Default::default() };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(!json.contains("year"), "{json}");
        assert!(!json.contains("links"), "{json}");
        let ancienne = r#"{"source":"radio","volume":50,"muted":false,"standby":false,"preset":null,"preset_count":null,"preset_name":null,"artist":null,"title":null,"album":null,"duration_s":null,"origin":null}"#;
        let relue: PlayerState = serde_json::from_str(ancienne).unwrap();
        assert_eq!(relue.morceau.year, None);
        assert!(relue.morceau.links.is_empty());
    }

    #[test]
    fn playback_ne_voyage_pas_quand_il_est_arrete() {
        // L'idiome additif : la valeur par défaut est absente du JSON, donc les
        // trames d'avant ce champ sont inchangées à l'octet.
        let etat = PlayerState::default();
        let json = serde_json::to_string(&etat).unwrap();
        assert!(!json.contains("playback"), "playback ne devrait pas etre serialise: {json}");
    }

    #[test]
    fn playback_voyage_en_minuscules_quand_il_dit_quelque_chose() {
        for (p, attendu) in
            [(Playback::Playing, "\"playback\":\"playing\""), (Playback::Paused, "\"playback\":\"paused\"")]
        {
            let etat = PlayerState { playback: p, ..Default::default() };
            let json = serde_json::to_string(&etat).unwrap();
            assert!(json.contains(attendu), "{attendu} absent de {json}");
            let retour: PlayerState = serde_json::from_str(&json).unwrap();
            assert_eq!(retour.playback, p);
        }
    }

    #[test]
    fn une_trame_sans_playback_se_relit_en_arret() {
        // Compatibilité descendante : une trame ecrite avant ce champ.
        let etat: PlayerState = serde_json::from_str(
            r#"{"source":"radio","volume":40,"muted":false,"standby":false,"preset":null,"preset_count":null,"preset_name":null}"#,
        )
        .unwrap();
        assert_eq!(etat.playback, Playback::Stopped);
    }

    #[test]
    fn enrichment_porte_une_position() {
        let e = Enrichment {
            identity: json!({"kind": "stream"}),
            position_s: Some(42),
            ..Default::default()
        };
        let back: Enrichment = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back.position_s, Some(42));
        let sans = r#"{"identity":{"kind":"stream"}}"#;
        assert_eq!(serde_json::from_str::<Enrichment>(sans).unwrap().position_s, None);
    }

    #[test]
    fn known_fait_un_aller_retour_et_se_relit_absent() {
        let np = NowPlaying {
            source: "files".into(),
            identity: Some(json!({"kind": "file", "path": "/mnt/nas/a.flac"})),
            known: Known {
                artist: Some("Lou Reed".into()),
                title: Some("Oooh Baby".into()),
                album: None,
                duration_s: Some(218),
                year: Some(1972),
                cover: true,
                // Valeur non-défaut, comme les champs voisins : ce test verifie
                // un aller-retour complet, et un `None` par defaut n'aurait rien
                // distingue d'un champ oublie dans l'implementation.
                stream_title: Some("Lou Reed - Oooh Baby".into()),
            },
        };
        let back: NowPlaying = serde_json::from_str(&serde_json::to_string(&np).unwrap()).unwrap();
        assert_eq!(back, np);

        // Une trame ecrite par un binaire anterieur n'a pas de `known` : elle
        // doit se relire, sinon la refonte ne peut pas se deployer greffon par
        // greffon.
        let ancienne = r#"{"source":"radio","identity":{"kind":"stream"}}"#;
        let relue: NowPlaying = serde_json::from_str(ancienne).unwrap();
        assert_eq!(relue.known, Known::default());
        assert!(!relue.known.cover);
    }

    #[test]
    fn known_vide_reste_muet_a_la_serialisation() {
        // Contrainte dure du chantier : une trame qui ne dit rien de connu doit
        // rester identique a l'octet pres a ce qu'elle etait avant l'ajout de
        // ce champ, sans quoi chaque trame grossirait pour rien.
        let muette = NowPlaying { source: "radio".into(), identity: None, known: Known::default() };
        let json = serde_json::to_string(&muette).unwrap();
        assert!(!json.contains("known"), "{json}");

        let bavarde = NowPlaying {
            source: "files".into(),
            identity: None,
            known: Known { artist: Some("Lou Reed".into()), ..Default::default() },
        };
        let json = serde_json::to_string(&bavarde).unwrap();
        assert!(json.contains("known"), "{json}");
        let back: NowPlaying = serde_json::from_str(&json).unwrap();
        assert_eq!(back, bavarde);
    }

    #[test]
    fn cover_ref_a_deux_formes_distinctes() {
        let url = CoverRef::Url { url: "https://coverartarchive.org/release/x/front-500".into() };
        let json = serde_json::to_string(&url).unwrap();
        assert!(json.contains(r#""kind":"url""#), "{json}");
        assert_eq!(serde_json::from_str::<CoverRef>(&json).unwrap(), url);

        let chemin = CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() };
        let json = serde_json::to_string(&chemin).unwrap();
        assert!(json.contains(r#""kind":"path""#), "{json}");
        assert_eq!(serde_json::from_str::<CoverRef>(&json).unwrap(), chemin);
    }

    #[test]
    fn cleaned_refuse_une_url_qui_n_est_pas_https_vers_un_hote() {
        // Ces valeurs viennent du reseau : le champ `coverUrl` de la trame SSE
        // d'OUI FM est ecrit par un tiers, et c'est le coeur qui irait la
        // chercher. Sans ce filtre, une trame hostile fait emettre a l'appareil
        // une requete vers l'adresse de son choix sur le reseau local.
        for mauvaise in [
            "http://example.org/a.jpg",
            "https://192.168.1.1/admin",
            "https://[::1]/a.jpg",
            "file:///etc/shadow",
            "ftp://example.org/a.jpg",
            "pas une url",
            "",
            // Confusion userinfo : tout avant le `@` est un nom d'utilisateur,
            // pas l'hote — un navigateur irait bien sur evil.example.
            "https://user@evil.example/a.jpg",
            "https://",
            "https://localhost/a.jpg",
        ] {
            let e = Enrichment {
                identity: json!(1),
                cover: Some(CoverRef::Url { url: mauvaise.into() }),
                ..Default::default()
            }
            .cleaned();
            assert!(e.cover.is_none(), "acceptee a tort : {mauvaise:?}");
        }
        let bonne = Enrichment {
            identity: json!(1),
            cover: Some(CoverRef::Url { url: " https://coverartarchive.org/x/front-500 ".into() }),
            ..Default::default()
        }
        .cleaned();
        assert_eq!(
            bonne.cover,
            Some(CoverRef::Url { url: "https://coverartarchive.org/x/front-500".into() })
        );
    }

    #[test]
    fn cleaned_refuse_un_chemin_relatif_ou_sans_extension_dimage() {
        for mauvais in ["relatif/folder.jpg", "/mnt/nas/notes.txt", "/mnt/nas/folder", ""] {
            let e = Enrichment {
                identity: json!(1),
                cover: Some(CoverRef::Path { path: mauvais.into() }),
                ..Default::default()
            }
            .cleaned();
            assert!(e.cover.is_none(), "accepte a tort : {mauvais:?}");
        }
        for bon in ["/mnt/nas/Album/folder.jpg", "/mnt/nas/A/Cover.JPEG", "/x/front.webp"] {
            let e = Enrichment {
                identity: json!(1),
                cover: Some(CoverRef::Path { path: bon.into() }),
                ..Default::default()
            }
            .cleaned();
            assert!(e.cover.is_some(), "refuse a tort : {bon:?}");
        }
    }

    #[test]
    fn une_pochette_seule_reste_une_non_reponse_pour_le_texte() {
        // Meme convention que `duration_s` : une pochette seule ne doit pas
        // gagner l'arbitrage du texte.
        let e = Enrichment {
            identity: json!(1),
            cover: Some(CoverRef::Url { url: "https://coverartarchive.org/x/front-500".into() }),
            ..Default::default()
        };
        assert!(e.is_empty());
    }

    #[test]
    fn fill_only_fait_le_tour_et_vaut_faux_par_defaut() {
        // Le defaut est « ecrase » : c'est la regle actuelle du projet, et
        // c'est ce qui evite de toucher aux trois greffons livres.
        let sans: Enrichment = serde_json::from_str(r#"{"identity":{"k":1}}"#).unwrap();
        assert!(!sans.fill_only);
        let e = Enrichment { identity: json!(1), fill_only: true, ..Default::default() };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""fill_only":true"#), "{json}");
        assert!(serde_json::from_str::<Enrichment>(&json).unwrap().fill_only);
        // Muet quand faux : la trame d'un greffon qui ecrase ne grossit pas.
        let defaut = Enrichment { identity: json!(1), ..Default::default() };
        assert!(!serde_json::to_string(&defaut).unwrap().contains("fill_only"));
    }

    #[test]
    fn stream_title_absent_ne_grossit_pas_la_trame() {
        // Même contrat que `covers` et `known` : un champ neuf ne doit rien
        // changer à la trame la plus courante, sinon chaque trame par seconde de
        // lecture paie l'ajout.
        let json = serde_json::to_string(&Known::default()).unwrap();
        assert!(!json.contains("stream_title"), "{json}");
    }

    #[test]
    fn stream_title_voyage_quand_il_est_la() {
        let k = Known { stream_title: Some("Miles Davis - So What".into()), ..Default::default() };
        let json = serde_json::to_string(&k).unwrap();
        assert!(json.contains(r#""stream_title":"Miles Davis - So What""#), "{json}");
        assert_eq!(serde_json::from_str::<Known>(&json).unwrap(), k);
    }

    #[test]
    fn une_trame_dun_binaire_anterieur_se_relit() {
        let k: Known = serde_json::from_str(r#"{"title":"X"}"#).unwrap();
        assert_eq!(k.stream_title, None);
    }

    #[test]
    fn morceau_tait_la_pochette_quand_il_n_y_en_a_pas() {
        let json = serde_json::to_string(&PlayerState::default()).unwrap();
        assert!(!json.contains("cover_href"), "{json}");
        assert!(!json.contains("cover_origin"), "{json}");

        let etat = PlayerState {
            source: "files".into(),
            morceau: Morceau {
                cover_href: Some("/api/cover/1a2b3c".into()),
                cover_origin: Some("files".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(json.contains(r#""cover_href":"/api/cover/1a2b3c""#), "{json}");
        assert!(json.contains(r#""cover_origin":"files""#), "{json}");
    }
}

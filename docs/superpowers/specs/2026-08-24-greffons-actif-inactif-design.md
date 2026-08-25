# Activer et désactiver un greffon à chaud

## Le besoin

Depuis l'IHM d'admin, éteindre un greffon et le rallumer sans redémarrer
le cœur ni éditer un fichier en SSH. Le choix survit au redémarrage : il
est écrit dans `/etc/ritornello/plugins.toml`.

Éteindre veut dire **tuer le processus**, pas seulement l'ignorer : la
raison habituelle d'éteindre un greffon est le matériel qu'il tient —
`/dev/sr0` pour `cd`, l'evdev pour `generic-input`, un `/dev/ttyN` pour
`console`. Un greffon décâblé mais vivant ne rendrait rien.

## Ce qui existe déjà et qu'on réutilise

Le chantier « rendez-vous des greffons » a laissé en place tout le chemin
d'arrivée :

- le socket d'enregistrement reste ouvert pour la vie du processus
  (`register::accept_forever`), donc un greffon peut s'annoncer à tout
  moment ;
- `cabler_a_chaud` câble genre par genre, recalcule l'ordre d'arbitrage
  des `metadata` depuis le manifeste et remplace les lignes de statut ;
- le SDK délie ses sockets avant de les lier
  (`ritornello-plugin-sdk/src/server.rs:250`), donc une relance après une
  mort brutale ne bute sur aucun fichier rance.

**La réactivation n'a donc besoin d'aucun code neuf de câblage** : il
suffit de relancer le binaire. Ce chantier écrit ce qui manque en face —
le décâblage, la mort du processus, et la persistance du choix.

## 1. Le fichier

`PluginConfig` gagne un champ :

```rust
#[serde(default = "actif_par_defaut")]
pub enabled: bool,
```

**Absent vaut actif.** Aucun `plugins.toml` en service ne change de sens,
et `deploy/plugins.example.toml` reste tel quel.

### L'écriture préserve les commentaires

L'écriture passe par **`toml_edit`**, nouvelle dépendance du cœur, et non
par `toml`. `plugins.example.toml` est fait de commentaires — c'est là
qu'est documenté à quoi sert chaque greffon — et `deploy.sh` y *ajoute*
des blocs commentés sur un appareil déjà en service. Un aller-retour
`toml::to_string` les effacerait tous au premier basculement.

- désactiver : pose `enabled = false` dans le `[[plugin]]` dont le `name`
  correspond ;
- réactiver : **retire la clé**, plutôt que d'écrire `enabled = true`. Le
  fichier retrouve sa forme d'origine, et « pas de mention = allumé »
  reste vrai à la lecture comme à l'écriture.

Écriture `tmp` puis `rename` dans le même répertoire — l'idiome de la
maison (`ritornello-plugin-files/src/admin.rs:473`). Un `plugins.toml`
tronqué par une coupure de courant rendrait l'appareil muet au démarrage
suivant.

`/etc/ritornello` appartient déjà à `ritornello:` (`deploy/deploy.sh`) :
le cœur peut écrire, rien à changer au déploiement.

### Persister d'abord, agir ensuite

Si l'écriture échoue (droits, montage en lecture seule), le basculement
est **refusé** : rien ne bouge au runtime et l'IHM dit pourquoi. Un
greffon tué dont l'extinction n'est pas persistée reviendrait au prochain
démarrage — un mensonge silencieux, pire qu'un refus franc.

## 2. Le cœur

### Tuer un processus dont on n'a plus le `Child`

Aujourd'hui le `Child` est **déplacé** dans la future de supervision
poussée dans `plugin_waits` : personne ne peut plus le tuer. La future
devient :

```rust
plugin_waits.push(async move {
    let mut child = child;
    tokio::select! {
        _ = kill_rx => { /* SIGTERM, grâce, SIGKILL */ (nom, child.wait().await, true) }
        st = child.wait() => (nom, st, false)
    }
});
```

et la boucle garde une table `nom → oneshot::Sender<()>`. Le troisième
membre du tuple dit au bras `plugin_waits.next()` si la mort était
**voulue** — ligne « désactivé », pas de `warn!` alarmiste — ou subie.

Deux options écartées : mémoriser le pid et signaler à l'aveugle (on ne
distingue plus mort voulue et plantage, et un pid se réutilise) ; une
tâche superviseur par greffon (plus propre dans l'absolu, mais réécrit la
supervision et la remontée des sorties des huit greffons pour un besoin
qui n'existe qu'ici).

`SIGTERM` d'abord, via le helper `libc` déjà présent
(`ritornello-core/src/system.rs:103`, même précédent que mpv), puis
`SIGKILL` après 2 s si le processus s'attarde. SIGTERM plutôt que SIGKILL
seul parce qu'un greffon qui dessine sur une console pourra un jour
rendre l'écran ; aucun ne le fait aujourd'hui, et aucun n'a besoin de
handler pour que SIGTERM le termine.

### Relancer

La boucle ne garde aujourd'hui du manifeste que `ordre_manifeste`, une
liste de noms. Relancer demande l'`exec` : elle devient une table
`nom → exec` (ordre du fichier conservé, c'est lui qui arbitre les
`metadata`). Relancer, c'est alors `plugins::spawn` avec la langue
courante, une nouvelle future de supervision et son déclencheur ; le
greffon s'annonce, `cabler_a_chaud` fait le reste. **Aucun code neuf sur
le chemin de réactivation.**

### Décâbler, genre par genre

| genre | ce qu'il faut faire |
|---|---|
| `source` | nouveau `Core::remove_source(name)` : retire de la table et de `source_order` ; si c'était l'active, `Deactivate` puis bascule sur la suivante du cycle, ou **plus aucune** s'il n'en reste pas |
| `metadata` | le nom sort de `Gathered::announcements`, `set_metadata_order` recalculé en entier depuis le manifeste — le chemin qui existe déjà pour les annonces tardives |
| `display` | rien d'explicite : `relais_afficheur` sort de boucle au premier échec d'envoi, ce que la mort du processus provoque |
| `input` | rien d'explicite : `run_input_client` rend la main sur EOF |
| `admin` | retiré de `admin_backends`, donc `/plugins/<nom>/` répond un 404 franc au lieu d'attendre les 5 s du protocole d'admin, qui est sériel |
| statuts | toutes les lignes du nom remplacées par une seule ligne « désactivé » (`status::replace_plugin_lines`, écrit pour ça) |

**Aucune source restante est un état légitime** : `demande_active`
(`core.rs:1083`) tolère déjà l'absence, et le démarrage sans source est
accepté depuis l'enregistrement à chaud.

### Le garde-fou qui condamnerait

`register::un_greffon_vivant` fait échouer le démarrage quand plus aucun
processus ne vit. Avec tous les greffons désactivés, ce refus mettrait le
cœur en boucle de redémarrage systemd — IHM comprise, donc plus aucun
moyen de rallumer quoi que ce soit. **Le refus ne doit compter que les
greffons activés** : tout éteindre est une configuration, pas une panne.

Corollaire au démarrage : un greffon désactivé n'est pas lancé du tout,
mais il **reste listé** dans les statuts avec sa ligne « désactivé ».
Sans cette ligne, la page ne le montrerait plus et il serait
irrécupérable depuis l'IHM.

## 3. La commande HTTP

`PUT /api/plugins/:name/enabled`, corps `{"enabled": bool}`.

- nom absent du manifeste → refus catalogué (404), par le catalogue de
  refus i18n existant : aucune clé ne doit atteindre l'écran ;
- la couche HTTP valide et **persiste**, puis envoie l'ordre au cœur par
  un `mpsc` accompagné d'un `oneshot` d'accusé — l'idiome de `audio_tx`,
  `theme_tx`, `settings_tx`. À l'extinction, la réponse ne part qu'une
  fois le **décâblage fait** : la page ne doit pas se rafraîchir sur un
  état intermédiaire. Au rallumage, elle part une fois le **binaire
  lancé** — attendre son annonce, c'est attendre un démarrage de
  processus ; d'ici là la ligne dit « figé », qui veut exactement dire
  ça : lancé, pas encore annoncé ;
- `PluginStatus` gagne `disabled: bool`, additif et omis du JSON quand il
  est faux — l'idiome déjà employé pour `stalled`.

## 4. L'IHM

Le tableau des greffons de la page de configuration devient **une ligne
par nom** au lieu d'une ligne par couple (nom, genre) : nom, genres
joints (« source, metadata »), état, lien d'admin, interrupteur. La
désactivation porte sur le nom ; le tableau doit montrer l'unité qu'on
manipule.

Le `Switch` du kit existe déjà. L'état affiché privilégie « désactivé »
sur connecté / figé / injoignable : il n'y a plus de processus, les
autres états n'ont plus de sens.

Pas de dialogue de confirmation : l'action est réversible depuis la même
ligne, et `sonner` — déjà en place — notifie le résultat. Contrepartie
assumée : couper `generic-input` fait perdre la télécommande jusqu'à ce
qu'on la rallume depuis cette page.

Clés i18n ajoutées dans `crates/ritornello-core/src/locales/en.toml` et
`deploy/locales/core/fr.toml` ; le test Rust de parité en/fr et le test
web des clés utilisées les couvrent.

## 5. Tests

Unitaires (Rust) :

- `enabled` absent vaut actif ; `enabled = false` lu comme tel ;
- l'écriture préserve commentaires et ordre du fichier ;
- réactiver retire la clé ;
- une écriture qui échoue laisse le runtime intact ;
- `remove_source` : source active, dernière source, nom inconnu ;
- `un_greffon_vivant` ignore les désactivés ;
- la ligne de statut d'un désactivé.

Aller-retour fichier → manifeste → fichier : éteindre puis rallumer rend
le fichier à sa forme d'origine, et le greffon rallumé reprend sa place
— donc sa priorité d'arbitrage `metadata`.

Le cycle complet en processus n'est pas testable en l'état : il vit dans
la boucle `main`, qui n'est pas extraite. Ce que ce chantier ajoute est
couvert par morceaux — `register` pour l'annonce et le recâblage,
`remove_source` pour le décâblage, la route pour l'enchaînement
persistance-puis-ordre — et la vérification du tout se fait sur
l'appareil.

Web : une ligne par nom, l'interrupteur, l'appel `PUT`, la notification.

**Aucun test ne suppose une exécution rapide** — la classe de flake déjà
identifiée sur ce dépôt (un test « négatif » qui suppose qu'un délai
n'est pas écoulé).

## Hors périmètre

- désactivation par genre plutôt que par greffon ;
- ordre de démarrage réglable ;
- redémarrer un greffon sans passer par désactiver puis réactiver ;
- désactivation temporaire non persistée.

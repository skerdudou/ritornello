# ritornello — Robustesse & observabilité (priorités 1 & 2 de la revue)

Implémente les priorités 1 et 2 de `docs/superpowers/revue-consolidee-2026-07-23.md` :
rendre l'appareil (headless) diagnosticable et résilient aux pannes de plugin,
et corriger deux comportements de correction relevés.

Date : 2026-07-23 — Statut : validé

## Contexte

Le code est livré et testé, mais la revue consolidée a identifié que les vraies
faiblesses portent sur la **résilience opérationnelle d'un appareil sans écran** :
plugins non supervisés, page de statut figée au démarrage, erreurs avalées sans
log, IPC qui attend un timeout au lieu d'échouer vite, et deux bugs de
correction (affichage CD, réveil de veille). Cette livraison regroupe P1 (1.1–1.5)
et P2 (2.1–2.3). Calibrage : projet perso, LAN de confiance, mono-utilisateur ;
priorité à l'observabilité et à la robustesse, pas à l'esthétique.

## Décisions de cadrage

| Sujet | Décision |
|---|---|
| 1.1 Supervision | **Détecter + marquer + logguer** (page de statut vivante). **Pas** de redémarrage automatique (reporté à une éventuelle spec future). |
| 2.2 Réveil | Comportement **décidé par le plugin** via une méthode `wake()` (défaut = `activate()`) : la radio reprend au réveil/boot, le cd ne se lance pas tout seul. Pas de config admin (YAGNI). |
| 1.5 Ligne malformée | **Politique unique** sur les 4 protocoles : logguer + ignorer la ligne, garder la connexion. Radio : les deux moitiés en tâches indépendantes. |
| Portée | Un seul lot « robustesse & observabilité ». Aucune nouvelle fonctionnalité utilisateur au-delà de 2.2. |

## 1.1 — Supervision des plugins + page de statut vivante

Aujourd'hui `StatusState` (liste des plugins + `connected` + `active_source`)
est calculé une fois au démarrage, jamais réécrit ; les processus enfants des
plugins sont poussés dans `children: Vec<Child>` et jamais surveillés.

- `AppState.status` est déjà un `Arc<tokio::sync::RwLock<StatusState>>`. On garde
  ce même `Arc` et on en donne un clone à la boucle `main` pour qu'elle puisse
  **écrire** l'état à chaud.
- La boucle `select!` de `main.rs` surveille la terminaison de **chaque** processus
  plugin, via un `FuturesUnordered<impl Future>` où chaque entrée attend
  `child.wait()` et porte le **nom** du plugin. Le `mpv_child.wait()` existant reste
  un bras dédié (sa mort reste fatale — relance par systemd). Quand un plugin
  enfant se termine : `tracing::warn!("plugin {name} termine: {status}")` et on
  passe `connected=false` pour ce plugin dans `StatusState`.
- La **source active** affichée suit les changements : après chaque commande
  susceptible de la changer (`SourceCycle`), la boucle met à jour
  `status.active_source` depuis `core.active_source()` (nouvel accesseur simple
  sur `Core`). (Aujourd'hui elle n'est écrite qu'au démarrage.)
- Pas de re-spawn ni de reconnexion : un plugin mort le reste jusqu'au
  redémarrage du service. Le lien admin d'un plugin marqué indisponible n'induit
  plus en erreur (la page montre `indisponible`).
- Les enfants restent détenus par la boucle (via `FuturesUnordered` qui possède
  les `Child`) pour préserver `kill_on_drop` à l'arrêt.

## 1.2 — Observabilité : logs sur les erreurs avalées

Ajout d'un `tracing::warn!` (ou `error!`) à chaque point où une erreur réelle
disparaissait sans trace :

- `ritornello-core/src/admin.rs` — les 3 handlers (`admin_page`, `admin_get_data`,
  `admin_put_data`) loguent l'erreur avant de renvoyer `502`
  (`warn!("plugin {name} admin injoignable: {e}")`).
- `ritornello-core/src/status.rs` — les 2 sites `list_devices().unwrap_or_default()`
  deviennent un `match`/`unwrap_or_else` qui logue l'échec `aplay -L`.
- `ritornello-core/src/core.rs` — la boucle `SetLocale` de `resume()` passe de
  `let _ = …` à un `if let Err(e) = … { warn!(…) }`, alignée sur `set_locale()`.
- `ritornello-core/src/main.rs` — la boucle `select!` gagne un traitement
  explicite du `Err(broadcast::error::RecvError::Lagged(n))` de `ev_rx.recv()` :
  `warn!("events en retard, {n} perdus")` au lieu d'un skip silencieux. (Le bras
  reste non fatal ; `Closed` reste traité comme aujourd'hui.)
- `ritornello-core/src/player/mpv.rs` — la ligne non-JSON lue sur la socket mpv
  est loguée en `debug!` avant d'être ignorée (debug, pas warn : peut arriver
  légitimement, mais on veut une trace en cas de besoin).

## 1.3 — IPC : drainer `pending` à la déconnexion

Dans `ritornello-plugin-sdk/src/client.rs`, les tâches lectrices de `SourceClient`
et `AdminClient` : à la sortie de la boucle `while let Ok(Some(line))` (EOF /
déconnexion), **vider `pending`** — chaque `oneshot::Sender` restant est simplement
`drop`é (ce qui fait résoudre le `rx.await` de `request()` en `Err(RecvError)`
immédiatement) avant le `warn!("connexion … fermee")` existant. Une requête en vol
au moment de la déconnexion échoue alors tout de suite au lieu d'attendre le
timeout de 5 s. `request()` mappe déjà `Ok(Err(_))` sur une erreur (« réponse
abandonnée ») — comportement conservé.

## 1.4 — i18n : packs embarqués

Dans `ritornello-i18n/src/lib.rs` :
- `Catalog::load` logue un `warn!("pack embarque {component} invalide: {e}")` si le
  parsing de l'anglais **embarqué** (`own_en` ou `COMMON_EN`) échoue — aujourd'hui
  silencieux via `unwrap_or_default()`. On factorise le parse en
  `try_parse(s) -> Result<HashMap<String,String>, toml::de::Error>` utilisé par
  `parse_pack` (silencieux, pour l'appelant qui gère) et par le chargement des
  couches de base (qui logue). `overlay_from_disk` continue de logguer sur TOML
  disque invalide (inchangé).
- Un test par crate qui embarque un `en.toml` (`ritornello-i18n` pour `COMMON_EN`,
  et — sans dépendre du disque — un test dans chaque plugin/cœur) vérifiant
  `assert!(!try_parse(EN_CONST).unwrap().is_empty())` (ou la présence des clés
  référencées). Objectif : une faute de syntaxe dans un pack embarqué casse un
  test au lieu de désactiver silencieusement une couche en prod.

## 1.5 — Politique unique « ligne malformée » + découplage radio

- `ritornello-plugin-sdk/src/server.rs` — `run_source_plugin`, `run_admin_plugin`,
  `run_display_plugin` : une ligne JSON invalide ne fait plus `?` (abort de la
  connexion) mais `warn!("ligne invalide ignoree: {e}")` + `continue`. La boucle
  se termine normalement (Ok) sur EOF. (Les erreurs d'**écriture** restent
  propagées : ce sont de vraies pertes de connexion.)
- `ritornello-plugin-sdk/src/client.rs` — `SourceClient`/`AdminClient` : la ligne
  invalide, aujourd'hui `continue` **silencieux**, gagne un `warn!` (cohérence avec
  `run_input_client` qui logue déjà). `run_input_client` inchangé.
- `ritornello-plugin-radio/src/main.rs` — remplacer `tokio::try_join!(source, admin)`
  par un lancement en **tâches indépendantes** : `tokio::join!` (sans court-circuit)
  ou deux `tokio::spawn` joints. Si l'une des deux se termine (erreur d'écriture,
  déconnexion), on logue mais on **n'interrompt pas** l'autre. Une panne sur la
  socket admin ne tue plus la lecture audio (et réciproquement).

## 2.1 — CD : suivi de piste

`ritornello-plugin-cd/src/main.rs` — `next_track`/`prev_track` :
- mettent à jour `self.track` (borné à `0..total_tracks` ; sans rebouclage au-delà
  des bornes — on reste sur la première/dernière piste, cohérent avec l'absence de
  retour d'index réel depuis le lecteur) ;
- renvoient `Some(self.view())` pour que l'affichage suive.

L'action envoyée au lecteur reste `PlayerNext`/`PlayerPrev` (inchangée). Note
assumée : sans notification « piste réellement changée » du lecteur, l'index
affiché est celui *demandé*, pas confirmé — acceptable et bien meilleur que
l'affichage figé actuel.

## 2.2 — Réveil décidé par le plugin (`wake`)

- `ritornello-proto/src/source.rs` — nouvelle variante `SourceReq::Wake`.
- `ritornello-plugin-sdk/src/server.rs` — le trait `SourcePlugin` gagne
  `async fn wake(&mut self) -> SourceOutcome { self.activate().await }` (défaut =
  se comporter comme `activate`, donc **jouer** — bon pour la radio et toute source
  simple) ; `run_source_plugin` dispatche `SourceReq::Wake => plugin.wake().await`.
- `ritornello-core/src/core.rs` — `resume()` (appelé au **démarrage** et à la
  **sortie de veille**) envoie `SourceReq::Wake` au lieu de `Activate`. Les chemins
  qui expriment un choix explicite de l'utilisateur (`SourceCycle`, `Select`,
  `retry_stream`) continuent d'utiliser `Activate` (jouer). Ainsi radio joue au
  réveil/boot, une source qui surcharge `wake` peut s'abstenir.
- `ritornello-plugin-cd/src/main.rs` — surcharge `wake()` : renvoie
  `SourceOutcome { action: SourceAction::Noop, view: Some(self.view()) }` — rafraîchit
  l'affichage (« pas de disque » / infos disque) **sans** émettre de `Play`.
- Le trait `Source` côté cœur (`core.rs`) route déjà n'importe quel `SourceReq` via
  `request` : `SourceReq::Wake` passe par le même canal, aucune autre modification
  de plomberie.

## 2.3 — mce : désambiguïsation du périphérique

`ritornello-plugin-mce/src/input.rs` — `find_device` :
- si la variable d'environnement `RITORNELLO_MCE_DEVICE` est définie, ouvrir
  **exactement** ce chemin (`/dev/input/eventN`), sans recherche ;
- sinon, recherche par sous-chaîne du nom comme aujourd'hui, mais `warn!` si
  **plusieurs** périphériques correspondent (en listant les candidats), avant de
  prendre le premier — pour diagnostiquer le cas fréquent des récepteurs MCE
  exposant deux nœuds.

## Erreurs / dégradation

Cohérent avec l'existant : tout reste best-effort, aucun de ces changements
n'introduit de chemin fatal nouveau (sauf mpv, déjà fatal→systemd). La supervision
ne fait que refléter et logguer ; l'IPC échoue plus vite mais pas plus souvent.

## Tests

- 1.1 : test que la mort d'un plugin (processus factice qui se termine) passe son
  `connected` à `false` dans `StatusState` — ou, si tester le `select!` complet est
  lourd, un test unitaire de la fonction qui met à jour `StatusState`
  (marquage par nom) + vérification manuelle bout-en-bout notée.
- 1.2 : pas de test dédié (ajouts de logs) ; on vérifie l'absence de régression.
- 1.3 : test que, connexion fermée côté serveur, une `request()` en vol renvoie
  une `Err` **avant** le timeout de 5 s (assert de rapidité via un délai court).
- 1.4 : test par crate `try_parse(EN_CONST)` non vide ; test que `try_parse` d'un
  TOML invalide renvoie `Err` (pour couvrir le nouveau chemin de log).
- 1.5 : test que `run_source_plugin`/`run_admin_plugin` **ignorent** une ligne
  invalide et **continuent** de répondre à la requête valide suivante (au lieu de
  fermer). Test radio : les deux serveurs de plugin démarrent et servent en
  parallèle (déjà partiellement couvert ; ajouter qu'une erreur simulée d'un côté
  n'empêche pas l'autre de répondre, si praticable).
- 2.1 : test que `next_track` incrémente `self.track` (borné) et renvoie une `View`
  reflétant la nouvelle piste.
- 2.2 : test SDK que `SourceReq::Wake` appelle `wake()` (fausse source enregistrant
  l'appel) ; test cd que `wake()` renvoie `Noop` + vue sans `Play` ; test cœur que
  `resume()` envoie `Wake` (fausse source), et roundtrip JSON de `SourceReq::Wake`.
- 2.3 : `find_device` — test de la sélection par `RITORNELLO_MCE_DEVICE` (chemin
  forcé) et du parsing/warn multi-candidats (fonction de sélection pure séparée de
  l'ouverture réelle du périphérique si nécessaire pour la testabilité).

## Hors périmètre

- Redémarrage/reconnexion automatique d'un plugin mort (1.1 option B) — éventuelle
  spec future.
- Rendre le comportement de réveil configurable dans l'admin d'un plugin (2.2) —
  YAGNI ; la distinction radio/cd par `wake()` suffit.
- Les points de priorité 3 et 4 de la revue (dette légère, couverture de tests
  additionnelle) et la checklist matériel — non inclus.
- Consolidation des trois stockages de « langue courante » (P3) — non inclus.

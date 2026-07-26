# ritornello — Revue de code consolidée & points d'amélioration

Date : 2026-07-23. Portée : tout le code généré (8 crates), après les 3 chantiers
(display+audio, serveur web unique, i18n). Consolide (a) une relecture fraîche
complète des fichiers actuels par 3 relecteurs (cœur / proto+sdk+i18n / plugins)
et (b) tous les follow-ups Minor déjà notés dans les ledgers.

**Calibrage** : projet perso, LAN de confiance, mono-utilisateur, appareil
**headless** (Raspberry Pi sans clavier/écran une fois déployé). Conséquence
directe sur les priorités : l'**observabilité** (pouvoir diagnostiquer depuis
`journalctl` quand quelque chose cloche sans écran) et la **robustesse aux
pannes de plugin/matériel** comptent plus que l'esthétique ; l'absence d'auth et
l'échappement HTML de config saisie par l'opérateur sont des non-sujets.

Aucun de ces points n'est bloquant — les 3 chantiers sont livrés, 89 tests verts,
clippy clean, cross-build ARM OK. C'est une liste de dette technique priorisée.

---

## Priorité 1 — Robustesse & observabilité (le plus utile sur un Pi headless)

### 1.1 Aucune supervision des plugins + page de statut figée
Le point le plus structurant, plusieurs facettes convergentes :
- `main.rs` — les enfants Source/Display/Input sont poussés dans `children: Vec<Child>`
  puis **jamais surveillés** (contrairement à `mpv_child` que la boucle `select!`
  observe via `.wait()`). Si un plugin crashe : personne ne le détecte, et le
  processus n'est jamais `reap`é (zombie possible jusqu'à l'arrêt du service).
- `status.rs` / `main.rs` — `StatusState` (`plugins[].connected`, `active_source`)
  est calculé **une fois au démarrage et jamais réécrit**. Un plugin qui meurt
  ensuite reste affiché « connecté », et le lien admin d'un plugin mort continue
  d'être rendu → clic = 502. La source active affichée ne suit pas non plus un
  `SourceCycle`.
- Déjà noté au ledger : « mort d'un plugin après démarrage jamais détectée/retentée »
  (hérité depuis le round display+audio).

**Piste** : ajouter les `.wait()` des enfants plugins à la boucle `select!` (un
`FuturesUnordered`), mettre à jour `StatusState` (le passer en `Arc<RwLock<…>>`
écrit par la boucle), et — idéalement — tenter un redémarrage/reconnexion avec
back-off. C'est le chantier le plus gros de cette liste ; il mériterait sa propre
spec.

### 1.2 Observabilité : erreurs avalées sans log
Regroupe plusieurs endroits où une erreur réelle disparaît sans trace (pénible
à diagnostiquer sans écran) :
- `admin.rs` — les 3 handlers (`admin_page`/`admin_get_data`/`admin_put_data`)
  renvoient `502` sur `Err(_)` **sans logger** l'erreur sous-jacente.
- `status.rs` — `audio_output::list_devices().unwrap_or_default()` (2 sites)
  avale un échec `aplay -L` (binaire absent, erreur ALSA) → liste vide sans log.
- `core.rs` — `resume()` pousse `SetLocale` aux sources avec `let _ = …`
  (silencieux), alors que `set_locale()` fait `tracing::warn!` sur le même échec.
  Incohérent, et masque un échec exactement au démarrage/réveil.
- `main.rs` — la boucle `select!` sur `ev_rx.recv()` (broadcast) ne matche que
  `Ok(ev)` : un `Err(Lagged(n))` (consommateur en retard) est **silencieusement
  ignoré**, alors que le back-off et le suivi de titre dépendent de voir tous les
  events. Ajouter un bras explicite qui logge le lag.
- `player/mpv.rs` — toute ligne non-JSON de la socket mpv est droppée sans log.
- `ritornello-i18n/src/lib.rs` — voir 1.4 (échec de parse d'un pack embarqué,
  silencieux).

**Piste** : un `tracing::warn!`/`error!` à chacun de ces points. Peu de code,
grand gain de diagnosticabilité.

### 1.3 IPC : la map `pending` n'est pas drainée à la déconnexion
`ritornello-plugin-sdk/src/client.rs` — dans `SourceClient::connect` et
`AdminClient::connect`, la tâche lectrice sort de sa boucle sur EOF/déconnexion
mais **ne draine pas `pending`**. Une requête en vol au moment où le plugin meurt
attend le **timeout de 5 s** au lieu d'échouer immédiatement, alors que la perte
de connexion est connue tout de suite. **Piste** : à la sortie de la boucle
lectrice, drainer `pending` et drop/erreur chaque sender en attente (le `rx.await`
de `request()` résout alors en `Err` aussitôt).

### 1.4 i18n : échec silencieux d'un pack **embarqué** + aucun test
`ritornello-i18n/src/lib.rs` — `parse_pack` (utilisé pour les couches de base
`own`/`common` à partir des constantes `include_str!`) fait `unwrap_or_default()`
**sans log**, contrairement à `overlay_from_disk` qui warn sur TOML invalide. Si
un `en.toml` embarqué (`common_en.toml`, `RADIO_EN`, `CD_EN`, `core::EN`) contient
un jour une faute de syntaxe, la couche devient une map vide et **chaque `get()`
renvoie la clé brute** — panne i18n totale et silencieuse de cette couche, en
prod, sans panic ni log. Aggravant : **aucun test** n'assure que les vrais
fichiers embarqués parsent en une map non vide (seules des chaînes littérales
sont testées). **Piste** : `warn!` si le pack embarqué échoue à parser, + 1 test
par crate `assert!(!parse_pack(REAL_EN).is_empty())` (ou vérifiant les clés
référencées par le code).

### 1.5 `try_join!` couple les deux moitiés de radio + ligne malformée = mort de la connexion
- `ritornello-plugin-radio/src/main.rs` — `tokio::try_join!(run_source_plugin, run_admin_plugin)`
  court-circuite au premier `Err`. Or `run_source_plugin`/`run_admin_plugin`
  propagent une erreur dure (`?`) sur **toute** ligne JSON malformée ou échec
  d'écriture. Donc un hoquet sur la socket admin **tue la lecture audio en cours**
  (et réciproquement). Déjà noté à la revue finale du serveur web unique.
- Transverse (`ritornello-plugin-sdk`) : politique **incohérente** sur ligne
  malformée entre les 4 protocoles — les serveurs (`run_source/admin/display_plugin`)
  **abortent toute la connexion** via `?`, `SourceClient`/`AdminClient` **skippent
  en silence**, `run_input_client` **skippe avec un warn**. 3 politiques pour le
  même cas.

**Piste** : politique unique « logguer + ignorer la ligne, garder la connexion »
(adaptée à un pipe IPC local de confiance) ; côté radio, isoler les deux moitiés
(join sans court-circuit, ou log+`Ok(())` sur ligne fautive) pour qu'une socket
ne tue pas l'autre.

---

## Priorité 2 — Correctness à corriger ou confirmer

### 2.1 CD : `next_track`/`prev_track` désynchronise l'affichage
`ritornello-plugin-cd/src/main.rs` — `next_track`/`prev_track` envoient
`PlayerNext`/`PlayerPrev` mais **ne mettent pas à jour `self.track`** et renvoient
`view: None`. Aucun chemin protocole ne remonte l'index réel de piste au plugin,
donc après un saut de piste à la télécommande, `view()` continue d'afficher
l'ancienne piste. **Piste** : incrémenter/décrémenter `self.track` (borné à
`0..total_tracks`) et renvoyer `Some(self.view())` ; ou ajouter une notification
« piste changée » au protocole.

### 2.2 La veille oublie un `Stop` explicite (⚠️ confirmer l'intention)
`core.rs` — entrer en veille ne consulte/efface pas `self.stopped` ; en sortie de
veille, `resume()` appelle inconditionnellement `Activate`, ce qui **relance la
lecture** si la source renvoie `Play`. Donc : l'utilisateur fait `Stop`, éteint,
rallume → **ça rejoue**. À comparer avec `SourceCycle` qui reset explicitement
`self.stopped = false`. **À confirmer** : si non voulu, garder l'`Activate` de
`resume()` derrière `!self.stopped` (comme `retry_stream()` le fait déjà).

### 2.3 mce : désambiguïsation du périphérique evdev
`ritornello-plugin-mce/src/input.rs` — `find_device` prend le **premier** evdev
dont le nom contient la sous-chaîne. Les récepteurs IR de type MCE exposent
souvent **deux** nœuds `/dev/input/eventN` au nom similaire (consumer-control vs
émulation clavier) ; prendre le mauvais prive tout l'input d'événements, avec
seulement un log info. **Piste** : warn si >1 candidat, et/ou variable d'env pour
forcer le chemin exact du périphérique. (Lié au follow-up matériel « evtest sur
le vrai Pi ».)

---

## Priorité 3 — Qualité & maintenabilité (dette légère)

- **Trois stockages de « langue courante »** (`AppState.locale_current`,
  `Core.locale`, `PersistedState.locale`) mis à jour sur 2 chemins (handler HTTP
  sync + boucle cœur async). Converge, mais source de la petite **course au
  reload** (le `PUT /api/locale` pose `locale_current` et renvoie 204 avant que le
  cœur ait reconstruit le `Catalog`) et du `<html lang>`/labels temporairement
  désaccordés. Envisager une consolidation (une seule source de vérité).
- **Duplication** : boucle de push `SetLocale` dans `resume()` vs `set_locale()`
  (+ gestion d'erreur incohérente, cf. 1.2) → factoriser en helper ;
  `parse_pack` vs `overlay_from_disk` dupliquent `toml::from_str` → un
  `try_parse() -> Result<…>` partagé ; `StatusState` a un `Deserialize` manuel
  (~28 lignes) qui reproduit exactement le derive → dériver directement.
- **i18n `Catalog::get<'a>(&'a self, key: &'a str) -> &'a str`** lie inutilement
  la vie du résultat à `key` → piège pour un futur appelant avec clé dynamique
  (`get(&format!(...))`) tenue au-delà de l'appel. Signature correcte :
  `get<'a>(&'a self, key: &str) -> &'a str`.
- **Couche `common`** = nom conceptuel ET répertoire (`root.join("common")`) : un
  plugin nommé `"common"` collisionnerait. Commentaire de nom réservé, ou garde.
- **`AdminResult::Set { ok, error }`** peut représenter des états invalides
  (`ok:true,error:Some` / `ok:false,error:None`) ; `unwrap_or_default()` donne
  `Err("")` remonté tel quel en 422. Modéliser plus serré (sérialiser un vrai
  `Result<(), String>`) ou au moins `unwrap_or_else(|| "erreur inconnue")`.
- **`AdminPlugin::page() -> String` synchrone** sur un trait sinon async : oblige
  `RadioAdmin` à tenir deux locks de types différents (`tokio::RwLock<Stations>` +
  `std::RwLock<Catalog>`) côte à côte. Commenter pourquoi, ou passer `page()` en
  `async fn`.
- **`parse_available_locales` ne trie pas** les langues externes → ordre du
  `<select>` non déterministe (dépend de `read_dir`). `out[1..].sort()`.
- **`PUT /api/locale`** n'accepte/valide pas contre la liste connue (dégrade en
  anglais, 204 quand même) — sanctionné par la spec, mais aucun feedback UI.
- **`<html lang="fr">` de la page admin radio** codé en dur (la page de statut a
  un `lang` dynamique) → tokeniser pour cohérence.
- **E/S bloquantes en contexte async** (impact faible, cohérence) :
  `state::save`/`Stations::save` (write+rename sync), `ConsoleDisplay::show`
  (write/flush tty), `cd::drive_status` (open+ioctl dans la boucle `watch`, alors
  que `read_toc`/`eject` utilisent `spawn_blocking`). En bonus, `play_preset`
  écrit l'état en tenant le read-guard `stations`.
- **`args[i+1]` panique** avec un message obscur si `--socket`/`--admin-socket`
  est le dernier argument (partagé radio/cd/mce/console) → `.get(i+1).expect(…)`.
- **Échappement** : `escape_html` (cœur) et l'`innerHTML` de `index.html`
  n'échappent pas partout les guillemets → self-XSS stocké **théorique** sur la
  page admin (non-sujet sur LAN de confiance, à noter si jamais exposé).
- **Divers** : `Event::TrackChanged` est parsé/propagé mais no-op dans
  `handle_event` (plomberie morte — câbler ou retirer) ; `Core::active()` panique
  si `active_source` absent de `sources` (inaccessible aujourd'hui, footgun
  latent → `Result`) ; pas de doc-comments sur les types publics du protocole ;
  quelques `.clone()` évitables (plan-mandated) ; commentaire `# page d'admin`
  omis dans `radio/fr.toml`.

---

## Priorité 4 — Couverture de tests (lacunes notées)

- Aucun test que les **packs anglais embarqués** parsent non-vides (cf. 1.4).
- Chemins **timeout / échec d'écriture** des clients IPC non testés
  (`SourceClient`/`AdminClient`/mpv `command`).
- i18n : texte **anglais par défaut** (pack absent) seulement « smoke », pas
  vérifié en contenu ; `page()` radio ne teste qu'1/10 jetons (un
  `assert!(!html.contains("{{"))` renforcerait) ; `get_data`/`set_data(ok)` de
  l'admin non testés directement (couverts en bout-en-bout).
- Pas de test direct de `retry_stream`, ni de `run_input_client`/`DisplayClient`.
- Roundtrips proto : plusieurs n'assertent que l'égalité post-roundtrip, pas la
  chaîne JSON littérale du fil.

---

## À vérifier sur le matériel réel (non-code, porté depuis la v1)

Jamais validé faute de Pi/périphériques réels dans l'environnement de dev (WSL) :
- codes de touches de la télécommande MCE (`evtest`) → ajuster
  `keymap.rs` ; + nombre de nœuds evdev exposés (cf. 2.3).
  **La télécommande n'est PAS testable depuis WSL — vérifié le 2026-07-23, ne pas
  retenter.** Deux blocages : (a) le rattachement `usbipd` cale avant la lecture du
  descripteur (dmesg s'arrête à `new full-speed USB device`, aucun `idVendor`) ;
  (b) surtout, le noyau WSL a `# CONFIG_RC_CORE is not set` et aucun
  `drivers/media/rc/` — le pilote `mceusb` du sous-système infrarouge n'existe donc
  pas, aucun nœud `/dev/input/eventN` ne peut être créé (il faudrait recompiler un
  noyau WSL). Raspberry Pi OS embarque `mceusb` + rc-core en standard : le relevé
  se fait là-bas, avec `sudo evtest` puis un test bout-en-bout du plugin seul
  (`nc -U` sur sa socket reçoit les `Command` JSON, le cœur n'est pas nécessaire).
- format réel de `cd-discid --musicbrainz` sur le lecteur.
- métadonnées de titre ICY (radio) sur les vrais flux.
- `aplay -L` sur le vrai Pi (le sélecteur de sortie audio n'a jamais listé de
  vrais périphériques — `aplay` absent de WSL).
- sortie jack analogique, getty/tty1 pour l'affichage console.

---

## Synthèse

Le code est sain et bien testé sur les chemins heureux ; les vraies faiblesses
sont concentrées sur **la résilience opérationnelle d'un appareil sans écran** :
supervision des plugins (1.1), diagnosticabilité quand une erreur survient (1.2,
1.4), et robustesse IPC face aux pannes/déconnexions (1.3, 1.5). Si un seul
chantier de suite devait être fait, ce serait **la supervision des plugins +
page de statut vivante (1.1)**, qui absorbe aussi une partie de 1.2. Le reste est
de la dette légère, à traiter opportunément quand on touche les fichiers
concernés.

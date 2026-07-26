# ritornello — Annuaire de radios en ligne

Permettre d'ajouter une station depuis un annuaire communautaire en ligne
(Radio Browser) au lieu de saisir une URL à la main, et rendre la numérotation
des présélections automatique.

Date : 2026-07-23 — Statut : validé

## Contexte

Ajouter une station demande aujourd'hui de connaître et de saisir l'URL exacte
de son flux. L'idée initiale était de livrer des catalogues figés (France
d'après le DAB+, États-Unis) importables depuis l'IHM. **Elle a été abandonnée
en cours de conception, preuve à l'appui** : en vérifiant une première liste
écrite à la main, trois des quatre URLs de la famille Oui FM répondaient déjà
`302` au lieu du flux. Un catalogue figé rouille, et le maintenir à jour serait
un travail sans fin.

**Radio Browser** (`https://api.radio-browser.info`) répond exactement à ce
besoin : annuaire libre et communautaire, API JSON publique **sans clé**,
filtrable par pays, qui suit lui-même l'état des flux (`hidebroken`) et expose
l'URL après redirections (`url_resolved`). Vérifié depuis la machine de
développement : la liste de serveurs et une requête « France, triée par
popularité » renvoient des stations actuelles et jouables.

## Décisions de cadrage

| Sujet | Décision |
|---|---|
| Source des stations | **Annuaire en ligne uniquement** (Radio Browser). Pas de catalogue livré, pas d'import de fichier, pas d'export. |
| Qui interroge l'annuaire | **Le plugin radio**, pas le navigateur : la page se heurterait au CORS, et les pages d'admin du projet ne chargent **aucune ressource externe**. |
| Transport des résultats | Aucune extension du protocole d'admin : une opération `search` dans `SetData`, résultats exposés par `GetData` (même mécanique que le mode apprentissage du plugin input). |
| Ajout d'une station | Côté navigateur, dans la table en cours d'édition ; **rien n'est persisté avant « Enregistrer »** (cohérent avec le reste du projet). |
| Numérotation | **Automatique, par position** (1, 2, 3…). Le champ preset disparaît de la table. Pas de réordonnancement pour l'instant. |
| Limite | **9 présélections** (les chiffres de la télécommande) : au-delà, l'ajout est refusé avec un message clair. |
| Client de l'annuaire | Client **reqwest écrit à la main** plutôt que la crate officielle `radiobrowser` : elle dépend d'`async-std` (deux exécutifs asynchrones dans un même binaire armv7, coût réel sur un Pi 2), réclame `reqwest ^0.11` quand le workspace est en 0.12 (deux piles HTTP embarquées), et n'est plus publiée depuis octobre 2023. Son seul avantage réel — le repli automatique sur un autre serveur — est repris ici à la main. |
| Serveur de l'annuaire | **Liste ordonnée** de serveurs essayés jusqu'au premier qui répond (l'annuaire a été observé entièrement en panne pendant la conception) ; `RITORNELLO_RADIO_DIRECTORY` épingle un serveur unique. |
| Temps de réponse | **Budget global de 4 s** pour la recherche entière, partagé par tous les essais (2 s au plus par serveur), et non un délai par serveur appliqué N fois : le cœur abandonne toute requête d'admin au bout de **5 s**, une recherche plus longue ne serait jamais vue. |

## Interroger l'annuaire

Nouveau module `directory.rs` dans le plugin radio.

- **Choix du serveur, avec repli** : `all.api.radio-browser.info` est un
  enregistrement tournant ; on interroge des serveurs concrets, essayés **dans
  l'ordre** jusqu'au premier qui répond — `de1`, `de2`, `at1`, `nl1`, `fi1`
  (`.api.radio-browser.info`). Ce n'est pas une garantie et la liste est
  assumée comme telle : le parc de miroirs bouge avec le temps, un hôte disparu
  échoue vite (DNS ou connexion refusée, bien avant le plafond d'un essai) et
  on passe au suivant sans avoir entamé le budget — ce sont les serveurs
  **lents** qui coûtent, et c'est eux que le budget borne. La liste reste
  courte pour cette raison : au-delà, les derniers serveurs ne seraient de
  toute façon jamais atteints dès que la liaison traîne. Pas de découverte dynamique par `/json/servers` : ce
  point d'entrée était lui-même en panne pendant la conception (« no available
  server »), il ne peut pas servir de filet. `RITORNELLO_RADIO_DIRECTORY`
  **épingle** un serveur : il devient alors le **seul** essayé, pour imposer
  son propre miroir sans recompiler. Si aucun serveur ne répond, un seul
  message d'erreur court remonte à la page, et chaque échec individuel est
  journalisé (`warn`, avec le serveur concerné) pour rester diagnosticable sur
  un Pi sans écran.
- **Politesse et temps de réponse** : en-tête `User-Agent: ritornello/<version>`
  (l'API le demande explicitement), un nombre de résultats borné (30), et
  surtout un **budget global de 4 s** pour la recherche entière — partagé par
  tous les serveurs essayés, et non un délai par serveur appliqué autant de
  fois qu'il y a de serveurs. Chaque essai reçoit `min(budget restant, 2 s)` ;
  dès qu'il ne reste plus de temps exploitable, on n'ouvre pas d'essai
  supplémentaire et l'erreur « aucun serveur n'a répondu » remonte
  immédiatement. Le pire cas est donc borné à ~4 s, quelle que soit la
  longueur de la liste (voir « Erreurs et dégradation » pour le pourquoi).
- **Requête** : `/json/stations/search` avec
  `name=<recherche>`, `countrycode=<FR|US|…>` (omis si « tous pays »),
  `hidebroken=true`, `order=clickcount`, `reverse=true`, `limit=30`.
- **Champs retenus** : `name`, `url_resolved` (à défaut `url`), `codec`,
  `bitrate`, `countrycode`. Une entrée sans URL exploitable est ignorée.

Séparation testable, sur le modèle déjà en place pour MusicBrainz dans le
plugin cd : une fonction **pure** `parse_search_results(json: &str) ->
Result<Vec<DirectoryStation>, String>` séparée de l'appel réseau, plus un
fichier d'exemple de réponse en `tests/fixtures/` pour que la suite de tests ne
touche jamais le réseau.

## Le flux côté IHM

1. L'utilisateur saisit une recherche et choisit un pays (France, États-Unis,
   tous), puis clique **Rechercher**.
2. Le navigateur envoie `SetData{op:"search", query, country}`. Le plugin
   interroge l'annuaire (l'appel est attendu dans l'opération : pas de sondage
   nécessaire, contrairement à l'apprentissage) et **mémorise** les résultats.
3. Le navigateur refait un `GetData` : les résultats arrivent dans un champ
   `search` — une liste de `{name, url, codec, bitrate, country}` — et
   s'affichent sous la recherche.
4. Chaque résultat porte un bouton **Ajouter** : il ajoute une ligne à la table
   des stations **côté navigateur**. Rien n'est écrit tant que l'utilisateur
   n'a pas cliqué **Enregistrer** (op `save` existante, inchangée).

`GetData` renvoie donc désormais `{stations, search}` ; `search` vaut une liste
vide tant qu'aucune recherche n'a été faite.

## Numérotation automatique

La colonne « présélection » disparaît de la table éditable. Le numéro affiché
est la position de la ligne (1 pour la première, 2 pour la deuxième…), et c'est
ce numéro que le navigateur écrit dans la charge utile envoyée à `save`.
Ajouter une station l'ajoute **en fin** de liste ; supprimer une ligne
renumérote les suivantes.

Conséquence assumée : supprimer la station 2 fait remonter les suivantes, donc
la touche 3 de la télécommande ne joue plus la même station qu'avant. C'est le
prix de la numérotation automatique, et c'est ce qui a été demandé.

La validation existante (`Stations::validate` : présélections uniques dans
1..=9) reste l'autorité côté serveur. L'IHM refuse d'ajouter une 10ᵉ station
avec un message traduit plutôt que de laisser la sauvegarde échouer.

## Erreurs et dégradation

- **Plafond de 5 s imposé par le cœur.** La page d'admin ne parle pas au plugin
  directement : le cœur relaie ses requêtes par la socket d'admin, et
  `AdminClient::request` (`crates/ritornello-plugin-sdk/src/client.rs`)
  enveloppe **tout** aller-retour dans un `timeout(5 s)`. Une opération `search`
  plus longue est perdue deux fois : le navigateur reçoit une erreur de timeout,
  et la réponse tardive du plugin est jetée. La recherche est donc
  **délibérément bornée en dessous** de ce plafond — budget global de 4 s, 2 s
  au plus par serveur — de sorte que l'échec remonte sous forme de message
  d'erreur traduit et exploitable, jamais sous forme de timeout du cœur. C'est
  aussi ce qui interdit un repli « aussi long qu'il le faut » : au-delà de 5 s,
  plus personne n'écoute la réponse.
- Un serveur injoignable, en délai dépassé ou répondant n'importe quoi ne fait
  pas échouer la recherche : on passe au serveur suivant de la liste (un miroir
  qui répond du JSON cassé est aussi inutile qu'un miroir muet). L'échec est
  journalisé serveur par serveur.
- Annuaire injoignable **en entier** (tous les serveurs muets), ou serveur
  épinglé en panne → message d'erreur traduit dans la zone de message de la
  page, **aucun plantage** ; les stations déjà configurées et la lecture en
  cours ne sont pas affectées. Le message reste court (un seul, pas la
  concaténation des causes) : le détail est dans le journal.
- L'appel réseau se fait dans l'opération `search` uniquement : la moitié
  Source du plugin (la lecture) n'est jamais bloquée par l'annuaire.
- Une réponse contenant des entrées inexploitables (URL vide) les ignore
  silencieusement plutôt que d'échouer en bloc.

## Tests

- `parse_search_results` : fixture de réponse réelle → stations attendues ;
  JSON invalide → `Err` ; entrée sans URL ignorée ; `url_resolved` préféré à
  `url`.
- Construction de l'URL de requête : paramètres attendus, `countrycode` omis
  quand « tous pays ».
- Liste des serveurs : ordre respecté, `RITORNELLO_RADIO_DIRECTORY` épingle un
  serveur unique, valeur vide ignorée. Test **pur** sur la construction de la
  liste : le repli réel (un serveur muet, le suivant qui répond) n'est pas
  testé, il demanderait le réseau ; seul l'épuisement de la liste l'est, avec
  des bases invalides qui échouent avant la moindre entrée/sortie.
- Budget : l'arithmétique est isolée dans une fonction **pure** qui calcule le
  délai d'un essai à partir du budget restant. Testée sans réseau ni horloge :
  budget intact → plafond par serveur ; budget entamé → ce qui reste ; budget
  épuisé → aucun essai de plus. Le test vérifie aussi l'invariant qui motive
  tout le dispositif — le budget global est strictement inférieur aux 5 s du
  client d'admin du cœur. **Aucun test temporisé** : rien qui dépende de la
  charge de la machine.
- Opération `search` : avec un analyseur alimenté par la fixture, les résultats
  se retrouvent bien dans `GetData` ; une erreur réseau donne un message
  traduit et laisse l'état intact. **Aucun test ne touche le réseau.**
- Numérotation : la charge utile envoyée à `save` attribue 1..N par position ;
  au-delà de 9, l'ajout est refusé.
- Parité des clés en/fr, comme partout ailleurs.

## Hors périmètre

- Catalogues locaux livrés, import et export de fichiers de catalogue.
- Réordonnancement des présélections (glisser-déposer, flèches).
- Voter / signaler une station à l'annuaire, favoris, historique.
- Recherche par genre ou par tag : la recherche par nom et le filtre pays
  suffisent pour l'usage visé.
- Mise en cache locale des résultats entre deux démarrages.

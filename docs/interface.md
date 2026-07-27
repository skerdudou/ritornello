# L'interface

## Télécommande web et API de commande

L'accueil (`http://<hôte>:8080/`) embarque une télécommande : les 11
commandes du protocole (présélections 1-9, suivant/précédent, volume, muet,
lecture/pause, stop, éjecter, changement de source, veille).

`Next`/`Prev` sont interprétées par la source active : présélection pour la
radio, piste pour le lecteur CD — ce n'est pas deux paires de commandes
distinctes, seulement une sémantique qui varie selon la source. Un binding
qui référence encore `NextTrack` ou `PrevTrack` (ancien nom) n'est plus
valide : il doit être réécrit en `Next`/`Prev`.

Elle passe par `POST /api/command`, dont le corps est exactement une commande
du protocole — le même canal que celui alimenté par les plugins Input, donc
aucune logique métier dupliquée :

    curl -X POST http://<hôte>:8080/api/command \
      -H 'content-type: application/json' -d '{"cmd":"VolumeUp"}'
    curl -X POST http://<hôte>:8080/api/command \
      -H 'content-type: application/json' -d '{"cmd":"Select","arg":3}'

Pratique pour piloter l'appareil sans télécommande (depuis un téléphone sur
le réseau local, ou en SSH pendant la mise au point).

L'encart **Lecteur** au-dessus de la télécommande (source active, volume,
muet, veille, et le morceau en cours avec l'origine de l'information) est
alimenté en flux poussé par `GET /api/player` (SSE) — rien n'est sondé, et
l'état suit la télécommande infrarouge comme les autres onglets.

## Télécommande physique

Si une touche ne répond pas, ouvrir `http://<hôte>:8080/plugins/generic-input/`,
choisir le périphérique dans la liste (bouton « Rafraîchir » s'il vient d'être
branché), cliquer « Apprendre » sur la ligne de l'action, appuyer sur la
touche, puis « Enregistrer ». Aucun redémarrage n'est nécessaire : la table
est relue à chaque appui. Pour partir d'une base, charger le preset `mce` ou
`keyboard`.

## Internationalisation (i18n)

L'interface est multilingue. La langue de base est l'**anglais**, embarquée
dans chaque binaire ; le français (et d'autres langues) sont fournis par des
**packs TOML externes**, décentralisés par composant :

    /etc/ritornello/locales/
      common/fr.toml   # vocabulaire commun (play/pause/stop/error…)
      core/fr.toml     # texte du cœur + page de statut
      radio/fr.toml    # plugin radio + page d'admin
      cd/fr.toml       # plugin cd
      <plugin-tiers>/fr.toml

- Racine configurable par `RITORNELLO_LOCALES` (défaut
  `/etc/ritornello/locales`).
- **Sélecteur** de langue sur la page de statut (`/status`) : il liste `en`
  plus tout pack `core/<lang>.toml` présent, chaque langue affichée par son
  nom dans sa propre langue (« Français », « English »). Le changement est
  appliqué à chaud, poussé aux plugins, et persisté (`state.json`).
- **Ajouter une langue** : copier l'`en` de référence, traduire les valeurs,
  le déposer sous `<root>/<composant>/<lang>.toml`. Une clé ou un pack
  manquant retombe automatiquement sur l'anglais (dégradation par clé, jamais
  d'erreur). Un pack présent mais illisible (droits, TOML invalide) est
  ignoré **avec une trace dans les journaux**.
- Les packs français initiaux sont livrés dans `deploy/locales/` et copiés
  par `deploy/deploy.sh`.

## Thème

L'interface propose une bascule **clair/sombre** et un sélecteur ouvrant une
popin avec les **42 thèmes** de [tweakcn](https://tweakcn.com) (Apache-2.0).
C'est un réglage **de l'appareil**, comme la langue : il est persisté dans
`state.json` (champs `theme` et `mode`) et s'applique donc à tous les
navigateurs qui consultent l'interface. Défaut : `northern-lights`, mode
clair.

Les polices déclarées par les thèmes sont chargées depuis un CDN — la seule
ressource externe de l'interface. Hors ligne, l'affichage retombe sur la
police système sans autre conséquence.

Régénérer les presets depuis l'amont :
`cd web/kit && node scripts/fetch-presets.mjs`.

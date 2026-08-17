# Assistants de déclaration de source — conception

*2026-08-16 — plugin `files`*

## 1. Le problème

La page du plugin `files` demande aujourd'hui à l'utilisateur de **savoir déjà**
ce qu'il veut déclarer. Pour ajouter un partage, il faut taper un nom de racine
conforme à une grammaire qu'on ne lui montre pas, une adresse, un nom de
partage, un sous-chemin — tout cela à l'aveugle, sans jamais vérifier qu'on a
visé juste. Le premier retour d'erreur arrive après avoir enregistré, puis
cliqué « Monter maintenant », et il parle de `mount.cifs`.

Pour un dossier de l'appareil, c'est pire : il faut connaître le chemin absolu
de la clé USB, que rien dans l'IHM n'affiche.

L'objectif est de renverser cela : **on parcourt d'abord, on déclare ensuite**.
On voit ce qu'on choisit, et le montage cesse d'être une étape que
l'utilisateur doit comprendre.

## 2. Décisions arrêtées avec le propriétaire

- **Volumes déjà montés uniquement.** On ne monte pas de périphérique bloc.
  Aucune capacité privilégiée nouvelle, le binaire racine ne bouge pas.
- **La popin déclare une source parcourable**, elle ne remplit pas la liste de
  lecture. Ce sont ensuite les sources déclarées qui offrent l'ajout.
- **Pas de bouton « Lire ».** Le SDK interdit délibérément à une Source de
  déclencher une lecture (`Notification` est sans action, et son commentaire
  explique pourquoi : une source qui joue de sa propre initiative rendrait la
  télécommande imprévisible). Le vocabulaire de `/api/command` n'a par ailleurs
  aucune sélection de source par nom, seulement `SourceCycle`. Câbler « Lire »
  exigerait donc d'élargir le protocole et le cœur ; l'ergonomie de la liste de
  lecture reste par ailleurs bornée par ce que le format m3u sait garder. Le
  sujet est **écarté de ce chantier**, pas tranché.
- **Trois gestes seulement sur la liste** : ajouter un fichier, ajouter un
  répertoire (récursif), vider. Les trois opérations existent déjà côté plugin.
- **Le parcours de l'appareil n'est pas clôturé** à `/media` ou `/mnt` (voir §9).
- **La saisie manuelle survit en repli** dans la popin réseau (voir §5).

## 3. Ce que la page devient

`VoletRacines` perd son formulaire et devient `VoletSources` : une **liste de
sources déclarées**. Chaque ligne porte la cible (`/media/usb`,
`//192.168.1.20/musique/Albums`), l'état du montage pour un partage, un
interrupteur « inscriptible », et deux actions — **« Ajouter à la liste »** et
**retirer**. Au-dessus, deux boutons ouvrant chacun un `Dialog` : *dossier de
l'appareil* et *partage réseau*.

Le kit fournit `Dialog` : aucun composant modal à écrire.

Le bouton « Monter maintenant » disparaît de la barre principale. Le montage
devient une conséquence de la déclaration (§6), et non un geste à comprendre.
Il subsiste en réessai discret sur la ligne d'un partage dont le montage a
échoué — c'est le seul moment où il veut dire quelque chose.

## 4. Parcours de l'appareil

La popin ouvre sur la **liste des volumes**, jamais sur `/`.

Un module pur `volumes.rs` lit `/proc/mounts` et écarte les points de montage
dont le type de système de fichiers figure dans une **liste noire** de
pseudo-systèmes (`proc`, `sysfs`, `tmpfs`, `devtmpfs`, `cgroup*`, `debugfs`,
`squashfs`…).

> **Décision revue après la première mise à l'épreuve.** Cette section
> prescrivait au départ l'inverse — une liste *blanche* des types acceptés — au
> motif qu'une liste noire oublierait le prochain pseudo-système du noyau. Le
> raisonnement pesait le mauvais risque, et l'usage l'a démenti tout de suite :
> `/mnt/c` sous WSL est un `9p`, et un disque USB en NTFS monté par ntfs-3g
> apparaît en `fuseblk`. Aucun des deux n'était prévu, et chacun rendait **un
> vrai disque inatteignable, sans contournement**. Une liste noire incomplète,
> elle, ne laisse passer qu'une entrée parasite dans une liste de choix : visible,
> réversible, mineur. `overlay` est délibérément absent de la liste noire, car
> sur un système conteneurisé c'est la racine elle-même.

L'écart n'est pas cosmétique : sans lui, un « ajouter à la liste » lancé sur `/`
partirait balayer `/proc`. C'est bien ce que la liste noire garantit encore.

Depuis un volume, on descend **dans les dossiers seuls**. Le dossier
actuellement affiché annonce combien de fichiers audio il contient
directement — un seul nombre, celui du niveau ouvert, et non un compte par
enfant qui obligerait à ouvrir chaque sous-dossier pour l'établir. C'est cette
information qui dit qu'on est au bon endroit ; sans elle on choisit un dossier
en espérant.

Le parcours local est synchrone, comme le `browse` existant : un système de
fichiers local répond bien en deçà du plafond de 5 s du cœur.

**Garde d'évasion.** `path` vient du navigateur. La règle n'est plus « sous une
racine déclarée » — il n'y en a pas encore — mais : on canonise le chemin, on
cherche parmi **tous** les points de montage celui qui le contient (le plus long
préfixe), et on n'accepte que si le type de ce montage est dans la liste
blanche.

Cette formulation par plus long préfixe n'est pas un détail : un test naïf
« commence par un volume » accepterait `/proc/self/root` puisque `/proc`
commence par `/`, qui est un volume. La règle correcte désigne le montage
**propriétaire** du chemin, et écarte donc `/proc`, `/sys`, `/dev` et `/run` par
construction.

## 5. Parcours d'un partage réseau

Trois temps dans un seul panneau : **hôte** (IP ou nom DNS) et identifiants
facultatifs → **liste des partages** → **descente dans les dossiers** →
confirmation.

Le moteur est **`smbclient`, en espace utilisateur**. C'est ce qui rend possible
de parcourir *avant* de monter. Les alternatives ont été écartées : monter
provisoirement demanderait un privilège pour un simple coup d'œil, laisserait
des montages orphelins si l'onglet se ferme, et surtout **ne saurait pas
énumérer les partages d'un hôte** — `mount.cifs` exige déjà de connaître le nom
du partage, ce qui est précisément la question qu'on veut poser à la machine.

- Partages : `smbclient -L //hôte -g`, dont la sortie est faite pour être
  analysée (`Disk|nom|commentaire`), là où le tableau humain change de largeur
  selon les versions.
- Dossier : `smbclient //hôte/partage -D <chemin> -c 'ls'`. Le répertoire de
  départ passe par `-D` plutôt que par un `cd "…"` inséré dans la chaîne de
  commande : un nom contenant un guillemet casserait l'analyse de `smbclient`.
- Utilisateur vide → tentative invité (`-N`). Beaucoup de NAS domestiques
  exposent un partage public, et exiger un compte les rendrait inaccessibles.
- Les partages administratifs (`IPC$`, `print$`, tout nom finissant par `$`)
  sont écartés de la liste : ils ne contiennent pas de musique et leur présence
  ferait douter du bon partage.

Trois points structurants :

**Le mot de passe ne passe jamais par `argv`.** Il serait lisible dans `ps` par
tout utilisateur de la machine. Il transite par un fichier d'authentification
temporaire créé en `0600` **à la création** (même règle que `ecrire_identifiants`
aujourd'hui : créer puis restreindre laisse une fenêtre), effacé après l'appel.

**La connexion est asynchrone.** Un NAS éteint fait attendre `smbclient`
jusqu'à son propre délai, largement au-delà du plafond de 5 s du cœur : une
requête admin bloquante serait tuée, et la page ne verrait jamais ni succès ni
échec. `SmbConnect` et `SmbBrowse` lancent donc une tâche et rendent la main
aussitôt ; la page suit l'avancement par sondage, exactement comme pour le
balayage. Une garde `tokio::time::timeout` tue le processus plutôt que de
compter sur un `-t` dont la présence varie selon les versions.

**Les identifiants vivent en session, pas dans la page.** Entre « se connecter »
et « confirmer », ils sont conservés en mémoire du plugin, indexés par hôte,
**jamais sérialisés dans `get_data`**, effacés à la fermeture de la popin et
après expiration. Le mot de passe traverse le fil une fois, pas à chaque clic
dans l'arborescence.

**Repli manuel.** La popin garde un mode de saisie directe hôte / partage /
sous-chemin / utilisateur. Il sert quand `smbclient` est absent (§8) et quand un
partage n'est pas énumérable — sans lui, ce chantier *retirerait* une capacité
qui existe aujourd'hui.

## 6. Déclaration d'une source

Une seule opération, `AddSource`, où débouchent les deux assistants **et** le
repli manuel. Elle remplace `SaveRoots`, qui faisait réécrire toute la table
depuis le navigateur — une lecture-modification-écriture dont on n'a plus besoin
dès lors qu'on ajoute une source à la fois.

**Le nom technique n'est plus saisi, il est dérivé.** `derive_name` replie les
accents, met en minuscules, remplace tout le reste par `-`, réduit les
répétitions, tronque à 32, et retombe sur `source` si rien d'exploitable ne
subsiste ; puis dédoublonne en `-2`, `-3`.

Ce nom devient **un composant du chemin de montage et un nom de fichier
d'identifiants**. La dérivation doit donc produire du valide *par construction*,
et non par chance : un test la nourrit de `../etc`, d'unicode, de la chaîne vide
et de 200 caractères, et exige que `nom_valide` accepte toujours le résultat.

Le dossier choisi devient le `subpath` ; la racine reste **le partage entier**,
puisque c'est lui qu'on monte. Chaque confirmation crée une source
indépendante : deux dossiers du même partage font deux sources, donc deux
montages du même partage — ce qui est légal, peu coûteux, et surtout sans
surprise. La solution « fusionner en élargissant le sous-chemin commun » a été
écartée : elle modifierait en silence la portée d'une source déjà déclarée.
Seul le doublon **exact** (même hôte, même partage, même sous-chemin) est
refusé.

**Le montage suit la déclaration.** `AddSource` écrit la table et les
identifiants, puis appelle `mount::reconcile` lui-même. Un échec ne défait pas
la déclaration — sinon l'utilisateur perdrait sa saisie à cause d'un NAS
endormi ; il est rapporté dans un champ `mount_error`, comme `scan.error`
rapporte l'échec d'un balayage terminé depuis longtemps.

Ce champ est **global et non porté par chaque source**, parce que
`systemctl start` réconcilie toutes les racines d'un coup et ne rend qu'un seul
résultat : prétendre attribuer cet échec à une source précise serait une
information inventée. Le détail par source reste le booléen `mounted`, lui
observé dans `/proc/mounts`.

`RemoveSource` retire la source, **efface son fichier d'identifiants** et
réconcilie. L'effacement manque aujourd'hui : retirer un partage laisse un
`.cred` contenant un mot de passe sur le disque.

`SetWritable` bascule l'inscriptibilité d'une source existante. Sans cette
opération, changer d'avis imposerait de retirer puis redéclarer, donc de
resaisir le mot de passe.

## 7. Ajout à la liste de lecture

Trois gestes, nommés identiquement partout :

- **« Ajouter à la liste »** sur une ligne de source — récursif sur toute la
  source.
- **« Ajouter à la liste »** sur un dossier de l'arbre — récursif.
- **« Ajouter »** sur un fichier.
- **« Vider la liste »**, qui reste où il est, dans le volet Liste.

Les opérations existent (`add_dir`, `add_file`, `clear`). Le travail est de les
rendre évidentes et de les nommer pareil aux deux endroits, pas d'en inventer.

## 8. Capacités et dégradation

`smbclient` absent **ne doit jamais faire planter ni échouer bruyamment**. Une
sonde au démarrage remplit un booléen `can_browse_smb` dans la charge utile ; la
page grise l'assistant réseau et nomme le paquet manquant, exactement comme
l'onglet Système grise le redémarrage sur `can_reboot`. La saisie manuelle, elle,
reste offerte : elle n'a besoin que de `cifs-utils`.

La sonde est refaite à la tentative de connexion, pour qu'installer le paquet
sans redémarrer le service donne un résultat juste plutôt qu'un refus périmé.

`cifs-utils` absent se manifeste déjà par l'erreur de `mount`, rapportée
verbatim. Rien à ajouter, mais la documentation doit le nommer (§14).

## 9. La frontière de sécurité, assumée

Parcourir l'appareil signifie : **la page peut lister les noms de répertoires
partout où l'utilisateur `ritornello` peut lire.** C'est un élargissement réel
par rapport à l'existant, où le parcours était borné aux racines déclarées.

Ce qui le rend proportionné, et qui doit rester vrai :

- seuls des **noms** sortent — noms de dossiers, et noms de fichiers dont
  l'extension est audio. Aucun contenu de fichier n'est jamais lu ni servi ;
- `ProtectHome=true` dans l'unité rend déjà `/home` et `/root` vides pour le
  plugin ;
- les pseudo-systèmes de fichiers sont hors d'atteinte par construction (§4) ;
- la même page, derrière les mêmes garanties d'accès, sait déjà redémarrer et
  éteindre la machine.

Décision explicite du propriétaire : **pas de clôture artificielle** à `/media`,
`/mnt` et `/srv`. Une telle borne n'empêcherait rien de sérieux et rendrait
indéclarable une bibliothèque rangée ailleurs.

## 10. Corrections emportées au passage

**`subpath` cesse de passer par `champ_sur`.** Cette fonction refuse les espaces
et les virgules parce que ses valeurs atterrissent dans la ligne d'options de
`mount.cifs`, séparée par des virgules. Vérification faite dans
`mount_options.rs` : **le sous-chemin n'entre jamais dans cette ligne** — seuls
`host` et `share` y vont, et le point de montage vient de `mount_point()`, qui
ignore le sous-chemin.

La règle est donc à la fois trop stricte et mal motivée : elle rend
indéclarable un dossier « Ma Musique » pour une raison qui ne le concerne pas.
Un `sous_chemin_sur` dédié refuse l'absolu, `..`, `.`, les composants vides et
l'octet nul — et accepte espaces, virgules et accents.

Le défaut se voit peu aujourd'hui, parce qu'on saisit le sous-chemin à la main
et qu'on abandonne devant un refus ; il deviendrait constant avec un assistant
qui propose de choisir n'importe quel dossier d'un NAS.

**`admin.rs` est scindé.** Le fichier fait 800 lignes ; les opérations
d'assistant partiraient dessus. Elles vont dans `explore.rs`, avec leur état,
et `admin.rs` délègue.

## 11. Découpage en modules

| Module | Rôle | Pur ? |
|---|---|---|
| `volumes.rs` | analyse de `/proc/mounts`, liste blanche, montage propriétaire d'un chemin | oui |
| `smb.rs` | analyse des sorties `smbclient`, classement des erreurs `NT_STATUS_*`, sonde de présence | analyse pure, appels isolés |
| `roots.rs` | + `derive_name`, + `sous_chemin_sur` | oui |
| `explore.rs` | état et opérations des deux assistants | non |
| `admin.rs` | délègue les opérations d'assistant | non |

Côté page : `VoletSources.vue` (liste et boutons), `DialogueAppareil.vue`,
`DialoguePartage.vue`, et `ChoixDossier.vue` — l'arbre de choix, partagé par les
deux popins parce que descendre dans des dossiers est le même geste des deux
côtés, quelle que soit la machine qui répond.

Le chemin de `/proc/mounts` devient surchargeable par
`RITORNELLO_FILES_PROC_MOUNTS`, pour que les tests et le parcours e2e décrivent
des volumes sans en monter.

## 12. Charge utile admin

Ajouts à `get_data` :

```json
{
  "volumes": [{ "path": "/media/usb", "fstype": "vfat" }],
  "can_browse_smb": true,
  "explore": {
    "kind": "smb", "host": "…", "share": "…", "path": "…",
    "shares": [], "dirs": [], "audio_count": 0,
    "busy": false, "error": null
  },
  "mount_error": null,
  "roots": [{ "…": "…", "mounted": true }]
}
```

`explore` est un emplacement **distinct** de `browse` : la popin et le volet
Parcourir sont deux curseurs indépendants, et les faire partager un emplacement
ferait qu'ouvrir une popin réinitialiserait l'arbre derrière elle.

Les identifiants de session n'apparaissent dans aucun de ces champs. Comme pour
`Root` aujourd'hui, la garantie est portée par le type : la structure sérialisée
ne contient pas de champ mot de passe.

Opérations :

| Opération | Champs |
|---|---|
| `AddSource` | `kind`, `path?`, `host`, `share`, `subpath?`, `user`, `domain`, `password`, `writable` |
| `RemoveSource` | `name` |
| `SetWritable` | `name`, `writable` |
| `Mount` | — (réessai sur une source dont le montage a échoué) |
| `ExploreOpen` | `kind` |
| `ExploreClose` | — |
| `ExploreLocal` | `path` (absolu) |
| `SmbConnect` | `host`, `user`, `password`, `domain` |
| `SmbBrowse` | `share`, `path` |

`SaveRoots` disparaît.

Dans `AddSource`, **`password` vide veut dire « prends celui de la session, à
défaut celui déjà enregistré »**. C'est le prolongement de la règle existante :
la page ne peut pas renvoyer un secret qu'elle ne reçoit jamais, et l'assistant
ne doit pas le lui faire retaper à la confirmation alors qu'il vient de servir à
se connecter.

## 13. Tests

**Sans NAS ni privilège, intégralement.**

- `volumes.rs` : liste blanche, montage propriétaire par plus long préfixe,
  refus de `/proc/self`, points de montage à espace échappé (`\040`).
- `smb.rs` : analyse de sorties `-L -g` et `ls` **captées**, avec noms accentués
  et noms contenant des espaces ; classement de `NT_STATUS_LOGON_FAILURE`,
  `NT_STATUS_ACCESS_DENIED`, hôte injoignable ; sortie vide.
- `derive_name` : l'invariant — tout indice, si hostile soit-il, produit un nom
  que `nom_valide` accepte ; et deux indices identiques produisent deux noms
  distincts.
- `sous_chemin_sur` : accepte « Ma Musique », refuse `..`, l'absolu, `a//b`.
- `explore.rs` : le mot de passe n'apparaît dans aucune sortie de `get_data` ;
  fermer la popin efface la session.
- `AddSource` : un montage en échec laisse la source déclarée et rapporte ;
  `RemoveSource` efface le `.cred`.
- Page : un test par volet, plus le test bidirectionnel `i18nKeysUsed` déjà en
  place, plus la parité en/fr.
- e2e : le parcours *dossier de l'appareil* de bout en bout via un
  `/proc/mounts` fixture ; le parcours *partage réseau* via un faux `smbclient`
  posé dans le `PATH` du serveur d'essai, qui rend des sorties captées.

## 14. Livraison et documentation

`docs/plugins.md` gagne, dans la section du plugin, un paragraphe **prérequis
paquets** explicite, disant lequel dégrade quoi :

- `cifs-utils` — **requis** pour monter un partage. Sans lui, une source réseau
  se déclare mais ne se monte pas.
- `smbclient` — **facultatif**. Sans lui, l'assistant réseau est grisé et la
  déclaration passe par la saisie manuelle. Rien d'autre n'est affecté.

`docs/installation.md` reprend la même liste dans sa section des partages
réseau.

## 15. Hors périmètre

- Monter un périphérique bloc non monté (clé USB sur un système sans
  automontage). Décision du propriétaire : hors chantier.
- Découverte des hôtes du réseau (mDNS, WS-Discovery). On saisit l'adresse.
- « Lire ce dossier », et plus largement toute commande de lecture partant de la
  page — voir §2.
- Toute refonte de la liste de lecture : elle est bornée par ce que le m3u sait
  conserver, et le sujet est ouvert, pas mûr.

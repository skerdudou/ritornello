import { expect, test } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

/**
 * Racine de fixtures preparee par le harnais, **telle que le coeur la voit**.
 *
 * Elle est tiree au sort a chaque execution (repertoire jetable), donc elle ne
 * peut pas etre ecrite en dur ici ; et sous Windows le coeur tourne dans WSL,
 * ou le meme repertoire s'appelle `/mnt/c/...`. Taper le chemin Windows dans la
 * page ferait echouer la validation (`Roots::validate` veut un chemin absolu,
 * et `C:\...` n'en est pas un pour Linux) : c'est donc serve.mjs qui publie la
 * forme utile, dans le meme fichier d'etat que celui dont teardown.mjs se sert.
 *
 * Lu **dans le test** et non au chargement du module : le fichier n'est ecrit
 * qu'au demarrage du serveur web, qui suit la collecte des tests.
 */
function racineFixtures(): string {
  // Meme calcul que serve.mjs et teardown.mjs : `process.cwd()` vaut `web/app`
  // (npm y place le process pour un script `-w app`), le fichier d'etat vit a
  // la racine du depot, sous `target/`.
  const racineNative = process.cwd().replace(/[\\/]web[\\/]app$/, '')
  const etat = JSON.parse(
    readFileSync(join(racineNative, 'target', 'e2e-etat.json'), 'utf8'),
  ) as { mediaRoot: string }
  return etat.mediaRoot
}

// Un seul test, et c'est delibere : chaque etape s'appuie sur l'etat serveur
// laisse par la precedente (la racine declaree, puis la liste balayee, puis la
// liste enregistree). Les separer en autant de tests les rendrait dependants de
// leur ordre d'execution sans que rien ne le dise.
test('parcours du plugin files : racine locale, balayage, liste enregistrée, présélections', async ({
  page,
  request,
}) => {
  // Le balayage est sonde a la seconde, le changement de source relance une
  // vraie lecture mpv : la marge par defaut de 30 s est courte pour l'ensemble.
  test.setTimeout(120_000)
  const racine = racineFixtures()

  // --- La page de gestion, sur un bac a sable vraiment vide -----------------
  await page.goto('/plugins/files/')
  // Le module ESM du plugin est charge dynamiquement et resolu par l'import
  // map : c'est ce qu'aucun test unitaire ne peut verifier.
  await expect(page.locator('[data-volet-sources]')).toBeVisible()
  // Aucune source : la preuve que le harnais a bien detourne
  // `RITORNELLO_FILES_ROOTS` vers son repertoire jetable. Sans cela, une
  // execution sur une machine ou Ritornello est installe lirait — et
  // ecraserait — le `/etc/ritornello/media-roots.toml` du proprietaire.
  await expect(page.locator('[data-no-sources]')).toBeVisible()

  // --- Declarer un dossier de l'appareil, par l'assistant -------------------
  // Le coeur du chantier : on ne tape plus un chemin absolu qu'aucun ecran
  // n'affiche, on part des volumes montes. Le harnais en *decrit* un dans un
  // faux /proc/mounts, faute de pouvoir en monter un sans privilege.
  await page.locator('[data-add-device]').click()
  // Le contenu de la popin vit dans un portail : Playwright le voit (il
  // interroge le document), la ou `wrapper.find` d'un test unitaire ne le
  // verrait pas.
  await expect(page.locator('[data-volume]')).toHaveCount(1)
  await page.locator('[data-volume]').click()

  // La popin ne doit pas deborder de son propre cadre. `DialogContent` est une
  // grille, et la largeur minimale d'un enfant de grille vaut par defaut celle
  // de son contenu : un nom de dossier long poussait le panneau au-dela de son
  // fond blanc, et la barre de defilement comme les boutons se retrouvaient
  // peints dehors. jsdom n'a pas de moteur de mise en page et ne peut pas le
  // voir — c'est mesurable ici, et nulle part ailleurs.
  const popin = page.locator('[data-dlg-appareil]')
  await expect(popin.locator('[data-choix-dossier]').first()).toBeVisible()
  const debordement = await popin.evaluate(
    (el) => el.scrollWidth - el.clientWidth,
  )
  expect(debordement, 'la popin deborde horizontalement de son cadre').toBeLessThanOrEqual(1)
  // On descend dans `media`, le dossier des fixtures. `proc` n'est pas
  // proposable : le volume unique est le repertoire jetable, et la liste
  // blanche des systemes de fichiers ecarte les pseudo-systemes.
  await page.locator('[data-choix-dossier]', { hasText: 'media' }).first().click()
  await expect(page.locator('[data-audio-count]')).toBeVisible()
  await page.locator('[data-choisir]').click()
  // Un refus s'affiche verbatim dans `[data-message]` : l'exiger absent donne
  // un echec qui nomme la cause au lieu d'un simple « pas de source ».
  await expect(page.locator('[data-message]')).toHaveCount(0)
  await expect(page.locator('[data-source-row]')).toHaveCount(1)
  // Relu depuis le plugin, et pas seulement affiche : c'est le seul moyen de
  // prouver que la table a bien atteint `media-roots.toml`, la page pouvant
  // afficher la ligne qu'on vient de choisir meme si l'enregistrement a echoue.
  const apresRacines = await (await request.get('/plugins/files/api/data')).json()
  expect(apresRacines.roots).toHaveLength(1)
  // Le nom n'est plus saisi : il est **derive** du dernier segment du chemin.
  expect(apresRacines.roots[0].name).toBe('media')
  expect(apresRacines.roots[0].path).toBe(racine)
  // Le mot de passe ne traverse jamais vers le navigateur — garanti par le type
  // cote plugin (`Root` ne porte pas le champ), verifie ici de bout en bout.
  expect(JSON.stringify(apresRacines)).not.toContain('password')

  // --- Parcourir, puis ajouter le dossier recursivement ---------------------
  // Aucun rechargement ici, et c'est la regression que cette etape epingle : le
  // volet Parcourir demande son premier niveau depuis un observateur qui se
  // declenche **pendant** le rendu provoque par l'enregistrement. Tant que le
  // vol unique de la page couvrait aussi la relecture, cet observateur recevait
  // `null` et l'arbre restait vide indefiniment — mesure : `[data-tree-row]`
  // bloque a 0 pendant les 5 s d'attente. Le vol ne couvre plus que l'envoi.
  const rangees = page.locator('[data-tree-row]')
  // Un seul niveau demande a l'ouverture : la racine ne contient que `Album`,
  // et aucun fichier audio a son sommet.
  await expect(rangees).toHaveCount(1)
  await expect(rangees.first().locator('[data-tree-name]')).toHaveText('Album')
  // Deplier : l'arbre est paresseux, ce niveau-la n'a pas encore ete demande.
  await rangees.first().locator('[data-tree-toggle]').click()
  // Les listes de lecture viennent **avant** les pistes : c'est souvent elles
  // qu'on cherche dans un dossier d'album, et une liste noyee sous cent
  // fichiers ne se voit pas.
  await expect(page.locator('[data-tree-name]')).toHaveText([
    'Album',
    'tout.m3u',
    '01.mp3',
    '02.mp3',
    '03.mp3',
  ])
  // Le bouton « ajouter ce dossier » de la seule rangee qui soit un dossier.
  await page.locator('[data-add-dir]').click()
  // Le balayage est **asynchrone** cote plugin : `add_dir` rend la main avant
  // la fin de la marche, et le protocole d'admin ne pousse rien. C'est le
  // sondage a la seconde de la page qui fait arriver les pistes — on attend
  // donc l'etat observable, jamais un delai fixe.
  const pistes = page.locator('[data-track-row]')
  await expect(pistes).toHaveCount(3, { timeout: 30_000 })
  await expect(page.locator('[data-track-name]')).toHaveText(['01', '02', '03'])
  // Aucun incident de balayage, et aucune piste introuvable : les chemins que
  // le plugin a memorises se relisent depuis le processus qui les a ecrits.
  await expect(page.locator('[data-scan-error]')).toHaveCount(0)
  await expect(page.locator('[data-track-missing]')).toHaveCount(0)

  // Les durees se relevent en tache de fond, par l'en-tete des fichiers.
  //
  // Un balayage ne fournit aucune duree — seul un `#EXTINF` en porte — donc la
  // colonne affichait un tiret. Le plugin lit desormais les en-tetes, par lots,
  // et la page sonde le temps que ca dure. Les fixtures font 30 s : c'est cette
  // valeur qu'on doit voir arriver, ce qui prouve une lecture reelle et non une
  // valeur inventee.
  await expect(page.locator('[data-track-row]').first().locator('td').nth(2)).toHaveText(
    /0:(29|30|31)/,
    { timeout: 30_000 },
  )

  // --- Enregistrer, vider, recharger ---------------------------------------
  await expect(page.locator('[data-no-saved]')).toBeVisible()
  await page.locator('[data-playlist-name]').fill('parcours')
  // La destination reste le stockage interne : la racine de fixtures n'est pas
  // declaree inscriptible, et le plugin refuserait d'y ecrire.
  await page.locator('[data-save-playlist]').click()
  await expect(page.locator('[data-saved-pick] option')).toHaveCount(1)
  await expect(page.locator('[data-saved-pick] option').first()).toContainText('parcours')

  await page.locator('[data-clear]').click()
  await expect(pistes).toHaveCount(0)
  await expect(page.locator('[data-empty-playlist]')).toBeVisible()

  // --- Charger un m3u trouve en parcourant ----------------------------------
  // Un fichier de liste posé sur la source, avec des chemins relatifs a
  // lui-meme. Il apparait dans l'arbre **a part** des pistes, et porte une
  // action differente : il remplace la liste au lieu de s'y ajouter.
  // L'arbre est deja deplie par l'etape de parcours ci-dessus : le replier ici
  // ferait disparaitre la rangee qu'on cherche.
  const ligneM3u = page
    .locator('[data-tree-row]')
    .filter({ has: page.locator('[data-tree-name]', { hasText: 'tout.m3u' }) })
  await expect(ligneM3u).toHaveCount(1)
  // Et surtout pas l'action d'ajout d'une piste : le geste juste ne doit pas
  // etre un choix parmi deux.
  await expect(ligneM3u.locator('[data-add-file]')).toHaveCount(0)
  await ligneM3u.locator('[data-load-m3u]').click()
  await expect(pistes).toHaveCount(3)
  // Les chemins relatifs se sont resolus contre le repertoire du m3u : des
  // pistes marquees introuvables signaleraient une resolution contre le mauvais
  // repertoire.
  await expect(page.locator('[data-track-missing]')).toHaveCount(0)

  await page.locator('[data-clear]').click()
  await expect(pistes).toHaveCount(0)

  await page.locator('[data-load-playlist]').click()
  await expect(pistes).toHaveCount(3)
  // Les chemins ont survecu a l'aller-retour par le m3u interne : une liste
  // rechargee dont les pistes seraient marquees introuvables signalerait des
  // chemins relatifs la ou le stockage interne exige de l'absolu.
  await expect(page.locator('[data-track-missing]')).toHaveCount(0)
  await expect(page.locator('[data-track-name]')).toHaveText(['01', '02', '03'])

  // --- L'accueil : la grille suit la source active -------------------------
  await page.goto('/')
  await expect(page.locator('[data-source]')).toHaveText('radio')
  // `SourceCycle` : le coeur trie les sources par nom (`files`, `radio`) et
  // demarre sur celle que `state.json` a memorisee — `radio`. Un cycle mene
  // donc a `files`, un second ramene.
  const source = page.getByRole('button', { name: 'Source', exact: true })
  await source.click()
  await expect(page.locator('[data-source]')).toHaveText('files')
  // Trois pistes, trois numeros : le compte est declare par la moitie Source a
  // l'activation, et c'est lui qui arme la telecommande.
  await expect(page.locator('[data-preset-button]')).toHaveCount(3)
  await expect(page.locator('[data-preset-count]')).toContainText('3')
  // Pas de pagination sous dix preselections.
  await expect(page.locator('[data-preset-prev]')).toHaveCount(0)
  await expect(page.locator('[data-preset-next]')).toHaveCount(0)

  // Choisir une piste par son numero doit reellement y aller.
  //
  // C'est le defaut central de ce chantier, et seul un parcours avec un vrai
  // mpv pouvait le voir : le coeur chargeait le m3u par `loadfile`, que mpv ne
  // deplie qu'**apres** coup (mesure : `playlist-count` vaut 1, puis 3 apres un
  // `end-file`). Le `playlist-pos` enchaine tombait donc hors bornes, la
  // lecture repartait de la piste 1, et l'affichage perdait numero et nom.
  // `loadlist` deplie sur-le-champ. Aucun test unitaire ne pouvait l'attraper :
  // il n'y a pas de mpv dedans.
  await page.locator('[data-preset-button]').nth(1).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('2')
  await expect(page.locator('[data-player-preset-name]')).toHaveText('02')
  await page.locator('[data-preset-button]').nth(2).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('3')
  await expect(page.locator('[data-player-preset-name]')).toHaveText('03')

  // Stop, puis Lecture : la lecture repart sur la piste ecoutee.
  //
  // `stop` vide la liste de mpv, si bien que « basculer la pause » n'avait plus
  // rien a reprendre : la touche Lecture ne faisait rien du tout, sur toutes les
  // sources. Elle redemande desormais a la source active de jouer.
  //
  // Et un arret **garde la piste armee** a l'affichage — numero et nom — au lieu
  // de ne laisser qu'un statut nu : l'afficheur dit ainsi « rien ne joue, voila
  // ce qui repartira ». C'est ce que cette etape verifie de bout en bout.
  //
  // Ce qu'elle ne sait **pas** distinguer : arrete ou en lecture, l'affichage est
  // le meme sur ces fixtures (une sinusoide n'a aucune metadonnee, donc pas de
  // bloc « en cours de lecture » a observer). La discrimination reelle est portee
  // par les tests unitaires de `stop()` et de `PlayPause`.
  await page.getByRole('button', { name: 'Stop', exact: true }).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('3')
  await expect(page.locator('[data-player-preset-name]')).toHaveText('03')
  await page.getByRole('button', { name: 'Play/Pause', exact: true }).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('3')

  // Remettre le harnais dans l'etat ou on l'a trouve : les parcours partagent
  // un unique coeur et `files.spec.ts` s'execute **avant** `parcours.spec.ts`,
  // qui exige la radio active. La remise est verifiee, pas esperee.
  await source.click()
  await expect(page.locator('[data-source]')).toHaveText('radio')

  // --- L'assistant reseau, sans NAS ----------------------------------------
  // En dernier, et deliberement : declarer un partage demande une
  // reconciliation de montage qui echouera ici (ni polkit ni unite systemd
  // dans le bac a sable), et cet echec ne doit surtout pas defaire ce que les
  // etapes precedentes ont etabli.
  //
  // Le `smbclient` que le plugin trouve est celui du harnais, qui rend des
  // sorties **captees sur un vrai NAS**. C'est ce qui rend cette etape jouable
  // sur n'importe quelle machine tout en eprouvant l'analyse contre du reel.
  await page.goto('/plugins/files/')
  await page.locator('[data-add-share]').click()
  await page.locator('[data-host]').fill('192.168.1.15')
  await page.locator('[data-user]').fill('ritornello')
  await page.locator('[data-password]').fill('peu-importe')
  await page.locator('[data-connect]').click()
  // Deux partages, pas trois : `IPC$` porte le type `IPC|` et non `Disk|`, et
  // la ligne de bruit « SMB1 disabled » n'est pas un partage non plus.
  await expect(page.locator('[data-share]')).toHaveCount(2, { timeout: 30_000 })
  await page.locator('[data-share]', { hasText: 'music' }).first().click()
  // Un nom de dossier a espaces survit a l'analyse par la droite du `ls` :
  // c'est le cas qui condamne toute lecture par la gauche.
  await expect(page.locator('[data-choix-dossier]', { hasText: 'Yann Tiersen' })).toHaveCount(1)
  await page.locator('[data-choix-dossier]', { hasText: 'Yann Tiersen' }).click()
  await page.locator('[data-choisir]').click()

  // La source est declaree malgre l'echec du montage — c'est l'invariant :
  // perdre la saisie parce qu'un NAS dort serait la pire des reponses.
  await expect(page.locator('[data-source-row]')).toHaveCount(2)
  const apresPartage = await (await request.get('/plugins/files/api/data')).json()
  expect(apresPartage.roots).toHaveLength(2)
  const partage = apresPartage.roots.find((r: { kind: string }) => r.kind === 'smb')
  // Nom derive du partage, sous-chemin garde tel quel — espace compris, ce que
  // l'ancienne regle de validation refusait.
  expect(partage.name).toBe('music')
  expect(partage.subpath).toBe('Yann Tiersen')
  expect(partage.host).toBe('192.168.1.15')
  // Et le mot de passe n'est toujours nulle part dans ce que la page recoit.
  expect(JSON.stringify(apresPartage)).not.toContain('peu-importe')
})

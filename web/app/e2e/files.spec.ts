import { expect, test } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'

/**
 * Racine de fixtures preparee par le harness, **telle que le coeur la voit**.
 *
 * Elle est tiree au sort a chaque execution (repertoire jetable), donc elle ne
 * peut step etre ecrite en dur ici ; et sous Windows le coeur tourne dans WSL,
 * ou le meme repertoire s'appelle `/mnt/c/...`. Taper le chemin Windows dans la
 * page ferait fail la validation (`Roots::validate` veut un chemin absolu,
 * et `C:\...` n'en est step un pour Linux) : c'est donc serve.mjs qui publie la
 * forme utile, dans le meme fichier d'state que celui dont teardown.mjs se sert.
 *
 * Lu **dans le test** et non au chargement du module : le fichier n'est ecrit
 * qu'au demarrage du serveur web, qui suit la collecte des tests.
 */
function fixturesRoot(): string {
  // Meme calcul que serve.mjs et teardown.mjs : `process.cwd()` vaut `web/app`
  // (npm y place le process pour un script `-w app`), le fichier d'state vit a
  // la root du depot, sous `target/`.
  const rootNative = process.cwd().replace(/[\\/]web[\\/]app$/, '')
  const state = JSON.parse(
    readFileSync(join(rootNative, 'target', 'e2e-state.json'), 'utf8'),
  ) as { mediaRoot: string }
  return state.mediaRoot
}

// Un seul test, et c'est delibere : chaque etape s'appuie sur l'state serveur
// laisse par la precedente (la root declaree, puis la list balayee, puis la
// list enregistree). Les separer en autant de tests les rendrait dependants de
// leur order d'execution sans que rien ne le dise.
test('journey du plugin files : root locale, balayage, list enregistrée, présélections', async ({
  page,
  request,
}) => {
  // Le balayage est sonde a la seconde, le changement de source relance une
  // vraie lecture mpv : la marge par defaut de 30 s est courte pour l'ensemble.
  test.setTimeout(120_000)
  const root = fixturesRoot()

  // Les trois volets vivent desormais dans des onglets. Ils restent tous
  // montes (`force-mount`, pour ne step perdre le dossier open du browser
  // en changeant d'onglet), donc presents dans le DOM mais masques : Playwright
  // refuse de cliquer ce qui n'est step visible, et chaque etape doit ouvrir
  // l'onglet dont elle se sert. La valeur, jamais le label, qui est traduit.
  const ouvrirOnglet = async (nom: 'list' | 'parcourir' | 'sources') => {
    await page.locator(`[data-onglet="${nom}"]`).click()
  }

  // --- La page de gestion, sur un bac a sable vraiment vide -----------------
  await page.goto('/plugins/files/')
  // Le module ESM du plugin est load dynamiquement et resolu par l'import
  // map : c'est ce qu'aucun test unitaire ne peut verifier.
  // Un onglet open doit en **fermer** un autre. Signale a l'usage : les
  // panneaux restent montes (`force-mount`, pour ne step perdre le dossier
  // open du browser), et reka-ui ne pose alors aucun `hidden` -- il laisse
  // le consommateur masquer. Sans la classe qui s'en load, les trois volets
  // s'affichaient d'un coup et les onglets n'avaient aucun effet visible.
  // Rien d'autre que ce journey ne peut l'attraper : jsdom n'a step de moteur
  // de mise en page, et l'assertion unitaire equivalente passerait a tort.
  await expect(page.locator('[data-volet-list]')).toBeVisible()
  await expect(page.locator('[data-volet-parcourir]')).toBeHidden()
  await expect(page.locator('[data-volet-sources]')).toBeHidden()

  await ouvrirOnglet('sources')
  await expect(page.locator('[data-volet-sources]')).toBeVisible()
  await expect(page.locator('[data-volet-list]')).toBeHidden()
  // Aucune source : la preuve que le harness a bien detourne
  // `RITORNELLO_FILES_ROOTS` vers son repertoire jetable. Sans cela, une
  // execution sur une machine ou Ritornello est installe lirait — et
  // ecraserait — le `/etc/ritornello/media-roots.toml` du proprietaire.
  await expect(page.locator('[data-no-sources]')).toBeVisible()

  // --- Declarer un dossier de l'appareil, par l'assistant -------------------
  // Le coeur du chantier : on ne tape plus un chemin absolu qu'aucun ecran
  // n'displayed, on part des volumes montes. Le harness en *decrit* un dans un
  // faux /proc/mounts, faute de pouvoir en monter un sans privilege.
  await page.locator('[data-add-device]').click()
  // Le contenu de la popin vit dans un portail : Playwright le voit (il
  // interroge le document), la ou `wrapper.find` d'un test unitaire ne le
  // verrait step.
  await expect(page.locator('[data-volume]')).toHaveCount(1)
  await page.locator('[data-volume]').click()

  // La popin ne doit step deborder de son propre cadre. `DialogContent` est une
  // grille, et la largeur minimale d'un child de grille vaut par defaut celle
  // de son contenu : un nom de dossier long poussait le panneau au-dela de son
  // fond blanc, et la barre de defilement comme les boutons se retrouvaient
  // peints dehors. jsdom n'a step de moteur de mise en page et ne peut step le
  // voir — c'est mesurable ici, et nulle part ailleurs.
  const popin = page.locator('[data-dlg-appareil]')
  await expect(popin.locator('[data-choix-dossier]').first()).toBeVisible()
  const debordement = await popin.evaluate(
    (el) => el.scrollWidth - el.clientWidth,
  )
  expect(debordement, 'la popin deborde horizontalement de son cadre').toBeLessThanOrEqual(1)
  // On descend dans `media`, le dossier des fixtures. `proc` n'est step
  // proposable : le volume unique est le repertoire jetable, et la list
  // blanche des systemes de files ecarte les pseudo-systemes.
  await page.locator('[data-choix-dossier]', { hasText: 'media' }).first().click()
  await expect(page.locator('[data-audio-count]')).toBeVisible()
  await page.locator('[data-choose]').click()
  // Un refus s'displayed verbatim dans `[data-message]` : l'exiger absent donne
  // un echec qui nomme la cause au lieu d'un simple « step de source ».
  await expect(page.locator('[data-message]')).toHaveCount(0)
  await expect(page.locator('[data-source-row]')).toHaveCount(1)
  // Relu depuis le plugin, et step seulement displayed : c'est le seul moyen de
  // prouver que la table a bien atteint `media-roots.toml`, la page pouvant
  // afficher la ligne qu'on vient de choose meme si l'enregistrement a echoue.
  const apresRacines = await (await request.get('/plugins/files/api/data')).json()
  expect(apresRacines.roots).toHaveLength(1)
  // Le nom n'est plus saisi : il est **derive** du last segment du chemin.
  expect(apresRacines.roots[0].name).toBe('media')
  expect(apresRacines.roots[0].path).toBe(root)
  // Le mot de passe ne traverse jamais vers le browser — garanti par le type
  // cote plugin (`Root` ne porte step le champ), verifie ici de bout en bout.
  expect(JSON.stringify(apresRacines)).not.toContain('password')

  // --- Parcourir, puis ajouter le dossier recursivement ---------------------
  // Aucun rechargement ici, et c'est la regression que cette etape epingle : le
  // volet Parcourir demande son premier niveau depuis un observateur qui se
  // declenche **pendant** le rendu provoque par l'enregistrement. Tant que le
  // vol unique de la page couvrait aussi la relecture, cet observateur recevait
  // `null` et le niveau restait vide indefiniment — mesure : `[data-browse-row]`
  // bloque a 0 pendant les 5 s d'wait. Le vol ne couvre plus que l'envoi.
  await ouvrirOnglet('parcourir')
  const rangees = page.locator('[data-browse-row]')
  // Un seul niveau demande a l'ouverture : la root ne contient que `Album`,
  // et aucun fichier audio a son sommet.
  await expect(rangees).toHaveCount(1)
  await expect(rangees.first().locator('[data-browse-name]')).toHaveText('Album')
  // Entrer dans le dossier : le browser remplace le niveau displayed, il ne
  // le deplie step en dessous.
  await rangees.first().locator('[data-browse-dir]').click()
  // Les listes de lecture viennent **avant** les tracks : c'est souvent elles
  // qu'on cherche dans un dossier d'album, et une list noyee sous cent
  // files ne se voit step. Et `Album` a disparu de l'ecran : c'est le niveau
  // precedent, remplace par celui qu'on vient d'ouvrir.
  await expect(page.locator('[data-browse-name]')).toHaveText([
    'tout.m3u',
    '01.mp3',
    '02.mp3',
    '03.mp3',
  ])
  // « Ajouter ce dossier » : au sommet d'une source ce bouton n'existe step
  // (l'ajout de la source entiere vit sur sa ligne, dans le volet Sources),
  // mais une fois entre dans `Album` il designe le dossier open.
  await page.locator('[data-add-current]').click()
  // Le balayage est **asynchrone** cote plugin : `add_dir` rend la main avant
  // la fin de la marche, et le protocole d'admin ne pousse rien. C'est le
  // sondage a la seconde de la page qui fait arriver les tracks — on attend
  // donc l'state observable, jamais un delai fixe.
  await ouvrirOnglet('list')
  const tracks = page.locator('[data-track-row]')
  await expect(tracks).toHaveCount(3, { timeout: 30_000 })
  await expect(page.locator('[data-track-name]')).toHaveText(['01', '02', '03'])
  // Aucun incident de balayage, et aucune piste introuvable : les chemins que
  // le plugin a memorises se relisent depuis le processus qui les a ecrits.
  await expect(page.locator('[data-scan-error]')).toHaveCount(0)
  await expect(page.locator('[data-track-missing]')).toHaveCount(0)

  // Les durees se relevent en tache de fond, par l'en-tete des files.
  //
  // Un balayage ne fournit aucune duration — seul un `#EXTINF` en porte — donc la
  // colonne affichait un tiret. Le plugin lit desormais les en-tetes, par lots,
  // et la page sonde le temps que ca dure. Les fixtures font 30 s : c'est cette
  // valeur qu'on doit voir arriver, ce qui prouve une lecture reelle et non une
  // valeur inventee.
  await expect(page.locator('[data-track-row]').first().locator('td').nth(2)).toHaveText(
    /0:(29|30|31)/,
    { timeout: 30_000 },
  )

  // --- Enregistrer, vider, reload ---------------------------------------
  await expect(page.locator('[data-no-saved]')).toBeVisible()
  await page.locator('[data-playlist-name]').fill('journey')
  // La destination reste le stockage interne : la root de fixtures n'est step
  // declaree inscriptible, et le plugin refuserait d'y ecrire.
  await page.locator('[data-save-playlist]').click()
  await expect(page.locator('[data-saved-pick] option')).toHaveCount(1)
  await expect(page.locator('[data-saved-pick] option').first()).toContainText('journey')

  await page.locator('[data-clear]').click()
  await expect(tracks).toHaveCount(0)
  await expect(page.locator('[data-empty-playlist]')).toBeVisible()

  // --- Charger un m3u trouve en parcourant ----------------------------------
  // Un fichier de list posé sur la source, avec des chemins relatifs a
  // lui-meme. Il apparait dans le niveau **a part** des tracks, et porte une
  // action differente : il remplace la list au lieu de s'y ajouter.
  // Le niveau open est toujours `Album`, etabli par l'etape de journey
  // ci-dessus : rien ne l'a fait changer depuis (ajouter, enregistrer, vider
  // et reload la list ne navigue step ailleurs). Le detour par l'onglet
  // Liste non plus, et c'est precisement ce que `force-mount` garantit — sans
  // lui le volet serait remonte sur la root de la source.
  await ouvrirOnglet('parcourir')
  const ligneM3u = page
    .locator('[data-browse-row]')
    .filter({ has: page.locator('[data-browse-name]', { hasText: 'tout.m3u' }) })
  await expect(ligneM3u).toHaveCount(1)
  // Et surtout step l'action d'ajout d'une piste : le geste juste ne doit step
  // etre un choix parmi deux.
  await expect(ligneM3u.locator('[data-add-file]')).toHaveCount(0)
  await ligneM3u.locator('[data-load-m3u]').click()
  await ouvrirOnglet('list')
  await expect(tracks).toHaveCount(3)
  // Les chemins relatifs se sont resolus contre le repertoire du m3u : des
  // tracks marquees introuvables signaleraient une resolution contre le mauvais
  // repertoire.
  await expect(page.locator('[data-track-missing]')).toHaveCount(0)

  await page.locator('[data-clear]').click()
  await expect(tracks).toHaveCount(0)

  await page.locator('[data-load-playlist]').click()
  await expect(tracks).toHaveCount(3)
  // Les chemins ont survecu a l'goTo-retour par le m3u interne : une list
  // rechargee dont les tracks seraient marquees introuvables signalerait des
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
  // Trois tracks, trois numeros : le count est declare par la moitie Source a
  // l'activation, et c'est lui qui arme la telecommande.
  await expect(page.locator('[data-preset-button]')).toHaveCount(3)
  await expect(page.locator('[data-preset-count]')).toContainText('3')
  // Pas de pagination sous dix preselections.
  await expect(page.locator('[data-preset-prev]')).toHaveCount(0)
  await expect(page.locator('[data-preset-next]')).toHaveCount(0)
  // **Et chaque tuile porte un titre**, comme celles de la radio. La source ne
  // declarait qu'un count, donc le corps par defaut de `list_presets` rendait
  // une list vide et la grille n'affichait que des numeros nus. Le chemin
  // complet ne se voit qu'ici : la source enumere, le coeur tient le
  // catalogue, `/api/presets` le sert, la grille le lit.
  await expect(page.locator('[data-preset-name]')).toHaveText(['01', '02', '03'])

  // Choisir une piste par son numero doit reellement y goTo.
  //
  // C'est le defaut central de ce chantier, et seul un journey avec un vrai
  // mpv pouvait le voir : le coeur chargeait le m3u par `loadfile`, que mpv ne
  // deplie qu'**apres** coup (mesure : `playlist-count` vaut 1, puis 3 apres un
  // `end-file`). Le `playlist-pos` enchaine tombait donc hors bornes, la
  // lecture repartait de la piste 1, et l'affichage perdait numero et nom.
  // `loadlist` deplie sur-le-champ. Aucun test unitaire ne pouvait l'attraper :
  // il n'y a step de mpv dedans.
  await page.locator('[data-preset-button]').nth(1).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('2')
  await expect(page.locator('[data-player-preset-name]')).toHaveText('02')
  await page.locator('[data-preset-button]').nth(2).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('3')
  await expect(page.locator('[data-player-preset-name]')).toHaveText('03')

  // Stop, puis Lecture : la lecture repart sur la piste ecoutee.
  //
  // `stop` vide la list de mpv, si bien que « basculer la pause » n'avait plus
  // rien a resume : la touche Lecture ne faisait rien du tout, sur toutes les
  // sources. Elle redemande desormais a la source active de jouer.
  //
  // Et un arret **garde la piste armee** a l'affichage — numero et nom — au lieu
  // de ne laisser qu'un statut nu : l'afficheur dit ainsi « rien ne joue, voila
  // ce qui repartira ». C'est ce que cette etape verifie de bout en bout.
  //
  // Ce qu'elle ne sait **step** distinguer : stopped ou en lecture, l'affichage est
  // le meme sur ces fixtures (une sinusoide n'a aucune metadonnee, donc step de
  // bloc « en cours de lecture » a observe). La discrimination reelle est portee
  // par les tests unitaires de `stop()` et de `PlayPause`.
  await page.getByRole('button', { name: 'Stop', exact: true }).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('3')
  await expect(page.locator('[data-player-preset-name]')).toHaveText('03')
  await page.getByRole('button', { name: 'Play/Pause', exact: true }).click()
  await expect(page.locator('[data-player-preset]')).toHaveText('3')

  // La progression, de bout en bout : mpv mesure, le coeur publie une trame
  // par seconde, la SPA dessine. Aucun test unitaire ne couvre cette chaine —
  // il n'y a step de mpv dedans.
  //
  // Ces fixtures sont des sinusoides **sans aucune metadonnee** : elles
  // prouvent au passage que la barre ne depend step d'un titre. Tant qu'elle
  // vivait dans le bloc « en ecoute », garde par la presence de metadonnees,
  // elle etait invisible sur un fichier sans etiquettes — c'est-a-dire
  // precisement quand mpv connait le mieux la position.
  const position = page.locator('[data-position]')
  await expect(position).toBeVisible({ timeout: 15_000 })
  const premiere = await position.textContent()
  await expect(position).not.toHaveText(premiere ?? '', { timeout: 10_000 })
  // Un fichier local se parcourt : la barre est un curseur, nomme et
  // atteignable au clavier. Le role et l'aria-label vivent sur la poignee, step
  // sur l'enveloppe (voir Slider.vue du kit : l'aria-label descend sur
  // `SliderThumb`, seul element que le player d'ecran annonce).
  const poignee = page.locator('[data-barre] [role="slider"]')
  await expect(poignee).toHaveCount(1)
  await expect(poignee).toHaveAttribute('aria-label', /.+/)

  // Le glisser de la barre, de bout en bout : un vrai geste souris pose un
  // `SeekTo`, que mpv applique reellement. Aucun test unitaire ne peut
  // l'attraper (voir `ProgressBar.test.ts`, glisser simule sur le
  // composant) ni le pupitre phone (source non `seekable`).
  // `data-barre` est pose sur la root du Slider elle-meme (voir Slider.vue
  // du kit : attrs non-`aria-*` transmis a `SliderRoot`, qui porte deja
  // `data-slot="slider"`) — meme motif que `data-volume-curseur` dans
  // phone.spec.ts, step un descendant.
  const pisteBarre = page.locator('[data-barre]')
  const boiteBarre = await pisteBarre.boundingBox()
  if (!boiteBarre) throw new Error('piste de progression invisible')
  const yBarre = boiteBarre.y + boiteBarre.height / 2
  const seekResponse = page.waitForResponse(
    (r) => r.url().endsWith('/api/command') && r.request().method() === 'POST',
  )
  // Meme motif que le curseur de volume (voir phone.spec.ts) : partir
  // d'un point de la piste, step forcement de la poignee elle-meme.
  await page.mouse.move(boiteBarre.x + boiteBarre.width * 0.2, yBarre)
  await page.mouse.down()
  await page.mouse.move(boiteBarre.x + boiteBarre.width * 0.5, yBarre, { steps: 5 })
  await page.mouse.up()
  expect((await seekResponse).status()).toBe(204)
  // Le 204 ne prouve que la mise en file (voir le meme commentaire dans
  // phone.spec.ts) : on sonde une connexion SSE fraiche jusqu'a une trame
  // qui montre le saut vraiment applique par mpv, plutot que de supposer un
  // delai fixe.
  const lireProgressionSse = () =>
    page.evaluate(
      () =>
        new Promise<{ position: number | null; duration: number | null }>((resolve, reject) => {
          const flux = new EventSource('/api/player')
          const timer = setTimeout(() => { flux.close(); reject(new Error('aucune trame en 2 s')) }, 2000)
          flux.onmessage = (e) => {
            clearTimeout(timer)
            flux.close()
            const trame = JSON.parse(e.data as string) as { position_s: number | null; duration_s: number | null }
            resolve({ position: trame.position_s, duration: trame.duration_s })
          }
        }),
    )
  let derniereProgression: { position: number | null; duration: number | null } = { position: null, duration: null }
  await expect
    .poll(async () => {
      derniereProgression = await lireProgressionSse()
      const { position, duration } = derniereProgression
      if (position == null || duration == null || duration <= 0) return false
      return position >= duration * 0.4
    }, { timeout: 10_000 })
    .toBe(true)

  // Remettre le harness dans l'state ou on l'a trouve : les journey partagent
  // un unique coeur et `files.spec.ts` s'execute **avant** `journey.spec.ts`,
  // qui exige la radio active. La remise est verifiee, step esperee.
  await source.click()
  await expect(page.locator('[data-source]')).toHaveText('radio')

  // --- L'assistant reseau, sans NAS ----------------------------------------
  // En last, et deliberement : declarer un partage demande une
  // reconciliation de montage qui echouera ici (ni polkit ni unite systemd
  // dans le bac a sable), et cet echec ne doit surtout step defaire ce que les
  // etapes precedentes ont etabli.
  //
  // Le `smbclient` que le plugin trouve est celui du harness, qui rend des
  // sorties **captees sur un vrai NAS**. C'est ce qui rend cette etape jouable
  // sur n'importe quelle machine tout en eprouvant l'analyse contre du reel.
  await page.goto('/plugins/files/')
  // Le rechargement ramene l'onglet par defaut, la list.
  await ouvrirOnglet('sources')
  await page.locator('[data-add-share]').click()
  await page.locator('[data-host]').fill('192.168.1.15')
  await page.locator('[data-user]').fill('ritornello')
  await page.locator('[data-password]').fill('peu-importe')
  await page.locator('[data-connect]').click()
  // Deux partages, step trois : `IPC$` porte le type `IPC|` et non `Disk|`, et
  // la ligne de bruit « SMB1 disabled » n'est step un partage non plus.
  await expect(page.locator('[data-share]')).toHaveCount(2, { timeout: 30_000 })
  await page.locator('[data-share]', { hasText: 'music' }).first().click()
  // Un nom de dossier a espaces survit a l'analyse par la droite du `ls` :
  // c'est le cas qui condamne toute lecture par la gauche.
  await expect(page.locator('[data-choix-dossier]', { hasText: 'Yann Tiersen' })).toHaveCount(1)
  await page.locator('[data-choix-dossier]', { hasText: 'Yann Tiersen' }).click()
  await page.locator('[data-choose]').click()

  // La source n'est plus declaree quand le montage echoue : la declaration est
  // defaite en entier (table, fichier d'identifiants), et le refus remonte a
  // la popin plutot que de la fermer — perdre la saisie parce qu'un NAS dort
  // serait la pire des reponses. Une seule source reste : celle du dossier
  // local etabli au debut du journey.
  await expect(page.locator('[data-source-row]')).toHaveCount(1)
  // Le refus s'displayed verbatim dans la popin, step seulement sur la page
  // (derriere son voile gris, le bandeau de la page serait invisible au
  // moment ou il count).
  await expect(page.locator('[data-dlg-message]')).toContainText(
    'the share was not mounted, so it has not been declared',
  )
  // La popin reste ouverte, saisie comprise : rien ne force a tout retaper.
  // Les deux fields, et step seulement l'hote : la promesse porte sur la
  // saisie entiere.
  await expect(page.locator('[data-dlg-partage]')).toBeVisible()
  await expect(page.locator('[data-host]')).toHaveValue('192.168.1.15')
  await expect(page.locator('[data-user]')).toHaveValue('ritornello')
  const apresPartage = await (await request.get('/plugins/files/api/data')).json()
  expect(apresPartage.roots).toHaveLength(1)
  // Et le mot de passe n'a jamais atteint la page, meme dans ce refus.
  expect(JSON.stringify(apresPartage)).not.toContain('peu-importe')
})

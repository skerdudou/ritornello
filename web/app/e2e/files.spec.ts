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
  await expect(page.locator('[data-volet-racines]')).toBeVisible()
  // Aucune racine : la preuve que le harnais a bien detourne
  // `RITORNELLO_FILES_ROOTS` vers son repertoire jetable. Sans cela, une
  // execution sur une machine ou Ritornello est installe lirait — et
  // ecraserait — le `/etc/ritornello/media-roots.toml` du proprietaire.
  await expect(page.locator('[data-no-roots]')).toBeVisible()

  // --- Declarer la racine locale -------------------------------------------
  await page.locator('[data-add-local]').click()
  await page.locator('[data-root-name]').last().fill('fixtures')
  await page.locator('[data-root-path]').last().fill(racine)
  await page.locator('[data-save-roots]').click()
  // Un refus s'affiche verbatim dans `[data-message]` : l'exiger absent donne
  // un echec qui nomme la cause au lieu d'un simple « pas de racine ».
  await expect(page.locator('[data-message]')).toHaveCount(0)
  await expect(page.locator('[data-root]')).toHaveCount(1)
  // Relu depuis le plugin, et pas seulement affiche : c'est le seul moyen de
  // prouver que la table a bien atteint `media-roots.toml`, la page pouvant
  // afficher la ligne qu'on vient de saisir meme si l'enregistrement a echoue.
  const apresRacines = await (await request.get('/plugins/files/api/data')).json()
  expect(apresRacines.roots).toHaveLength(1)
  expect(apresRacines.roots[0].name).toBe('fixtures')
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
  await expect(page.locator('[data-tree-name]')).toHaveText([
    'Album',
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

  // Remettre le harnais dans l'etat ou on l'a trouve : les parcours partagent
  // un unique coeur et `files.spec.ts` s'execute **avant** `parcours.spec.ts`,
  // qui exige la radio active. La remise est verifiee, pas esperee.
  await source.click()
  await expect(page.locator('[data-source]')).toHaveText('radio')
})

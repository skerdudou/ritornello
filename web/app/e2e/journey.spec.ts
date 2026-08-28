import { expect, test } from '@playwright/test'

// Lit une variable CSS **calculee** sur la root du document : c'est la
// seule preuve qu'un moteur de thème agit reellement sur le rendu — un
// attribut ou une classe ne prouve que l'intention, step l'effet.
const variable = (page: import('@playwright/test').Page, nom: string) =>
  page.evaluate(
    (n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim(),
    nom,
  )

test('navigation entre l’accueil, la config et les pages de plugin', async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('[data-preset-button="1"]')).toBeVisible()
  // Le harness ne déclare qu'une seule station (stations.toml) : la grille
  // n'displayed que ce numéro reel, et step de fleches de pagination puisque le
  // count ne dépasse step neuf.
  await expect(page.locator('[data-preset-button]')).toHaveCount(1)
  await expect(page.locator('[data-preset-prev]')).toHaveCount(0)
  await expect(page.locator('[data-preset-next]')).toHaveCount(0)
  await page.goto('/config')
  // `getByText('radio')` seul est ambigu : l'en-tete list aussi les
  // plugins admin par leur nom (voir App.vue), donc « radio » y apparait en
  // plus de la cellule du tableau de statut — d'ou ce ciblage par role. Et
  // `exact` : la cellule de l'interrupteur s'appelle « Enable or disable
  // radio », que le nom partiel attrapait aussi.
  await expect(page.getByRole('cell', { name: 'radio', exact: true })).toBeVisible()

  // Une sauvegarde annonce son issue par une notification. Signale a l'usage :
  // la feuille de style de vue-sonner n'etait importee nulle part, si bien que
  // le message se rendait **dans le flux du document** -- un text nu en bas de
  // page, qu'il fallait faire defiler pour voir. Rien d'autre que ce journey ne
  // peut l'attraper : jsdom ne calcule aucun style, et l'assertion unitaire
  // equivalente passerait a tort.
  await page.locator('[data-seek-change]').click()
  const notif = page.locator('[data-sonner-toast]').first()
  await expect(notif).toBeVisible()
  // `fixed` : c'est la preuve que la feuille est chargee. Sans elle, le
  // conteneur reste en `static` et le message tombe au fil du document.
  const conteneur = page.locator('[data-sonner-toaster]')
  await expect(conteneur).toHaveCSS('position', 'fixed')
  // Centree en haut et coloree par type, comme demande : ces deux attributs
  // sont ce que vue-sonner pose d'apres les props du `<Toaster />`, et rien
  // d'autre dans l'IHM ne les porte.
  await expect(conteneur).toHaveAttribute('data-y-position', 'bottom')
  await expect(conteneur).toHaveAttribute('data-x-position', 'center')
  await expect(conteneur).toHaveAttribute('data-rich-colors', 'true')
  // Page de plugin : le module ESM est load dynamiquement et resolu par
  // l'import map — c'est ce qu'aucun test unitaire ne peut verifier.
  await page.goto('/plugins/radio/')
  await expect(page.locator('[data-save]')).toBeVisible()
  await page.goto('/plugins/generic-input/')
  // Vingt-trois depuis l'ajout des deux touches de deplacement dans la piste.
  // Ce count est le seul verrou qui count les lignes **rendues** : les tests
  // unitaires verrouillent la list `ACTIONS` en amont, mais aucun d'eux ne
  // mounted la page reelle.
  await expect(page.locator('[data-action-row]')).toHaveCount(23)
})

test('une seule instance de Vue sert le shell et les modules de plugin', async ({ page }) => {
  const requetes: string[] = []
  page.on('request', (r) => requetes.push(new URL(r.url()).pathname))
  await page.goto('/plugins/radio/')
  await expect(page.locator('[data-save]')).toBeVisible()
  // La propriete centrale de l'architecture : le shell et le module de
  // plugin importent tous deux 'vue' via l'import map, qui resout vers la
  // meme URL stable — une seule requete, donc une seule instance chargee.
  expect(requetes.filter((p) => p === '/assets/vue.js')).toHaveLength(1)
  expect(requetes).toContain('/plugins/radio/ui.js')
  expect(requetes).toContain('/plugins/radio/ui.css')
})

test('l’état du player arrive en flux poussé dès la connexion', async ({ page }) => {
  await page.goto('/')
  // Seule preuve de bout en bout de la route SSE : un `EventSource` reel du
  // browser, servi par le vrai binaire Rust. Aucun test unitaire ne couvre
  // la chaine complete (axum -> canal watch -> EventSource), et la propriete
  // verifiee ici est precisement celle qui evite un onglet vide : l'state
  // courant est emis **des la connexion**, sans attendre un changement.
  const premiere = await page.evaluate(
    () =>
      new Promise<string>((resolve, reject) => {
        const flux = new EventSource('/api/player')
        const timer = setTimeout(() => {
          flux.close()
          reject(new Error('aucune trame en 5 s'))
        }, 5000)
        flux.onmessage = (e) => {
          clearTimeout(timer)
          flux.close()
          resolve(e.data as string)
        }
      }),
  )
  // Le harness ne declare aucun plugin `metadata` et ne lit aucun flux reel :
  // l'state est donc vide de metadonnees, mais il porte deja source et volume —
  // c'est ce qui permet a l'IHM d'afficher l'state du player sans sondage.
  const state = JSON.parse(premiere) as {
    source: string
    volume: number
    muted: boolean
    standby: boolean
    preset: number | null
    title: string | null
  }
  expect(state.source).toBe('radio')
  expect(state.volume).toBeGreaterThan(0)
  expect(state.muted).toBe(false)
  expect(state.standby).toBe(false)
  // Le reveil de la radio joue la preselection 1 (stations.toml du harness) et
  // la declare : c'est la preuve de bout en bout que la touche active voyage
  // du plugin jusqu'a la SPA. FIP n'emet step d'ICY, donc step de titre.
  expect(state.preset).toBe(1)
  expect(state.title).toBeNull()
  // Et l'encart du player les displayed.
  await expect(page.locator('[data-source]')).toHaveText('radio')
  await expect(page.locator('[data-volume]')).toHaveText(`${state.volume} %`)
  await expect(page.locator('[data-now-playing]')).toHaveCount(0)
  // La touche de la preselection qui joue est mise en evidence, et elle seule.
  await expect(page.locator('[data-preset-button="1"]')).toHaveAttribute('data-preset-active', 'true')
  await expect(page.locator('[data-preset-active]')).toHaveCount(1)
})

test('bascule clair/sombre, appliquée et persistée', async ({ page }) => {
  await page.goto('/')
  const clair = await variable(page, '--background')
  await page.getByLabel('toggle theme mode').click()
  await expect.poll(() => variable(page, '--background')).not.toBe(clair)
  const sombre = await variable(page, '--background')
  // Persistance cote serveur : un rechargement doit conserver le mode.
  await page.reload()
  await expect.poll(() => variable(page, '--background')).toBe(sombre)
  expect(await page.evaluate(() => document.documentElement.classList.contains('dark'))).toBe(true)
})

test('choix d’un thème dans la popin, appliqué et persisté', async ({ page }) => {
  await page.goto('/')
  await page.getByLabel('pick theme').click()
  await page.locator('[data-preset="vercel"]').click()
  const primaire = await variable(page, '--primary')
  await page.reload()
  await expect.poll(() => variable(page, '--primary')).toBe(primaire)
  await page.getByLabel('pick theme').click()
  await expect(page.locator('[data-preset="vercel"]')).toHaveAttribute('data-active', 'true')
})

test('la popin list les 42 thèmes et les filtre', async ({ page }) => {
  await page.goto('/')
  await page.getByLabel('pick theme').click()
  await expect(page.locator('[data-preset]')).toHaveCount(42)
  // Frappe reelle plutot qu'un `setInputFiles`/evaluation directe : le
  // `v-model` du composant `Input` du kit passe par `useVModel({passive:
  // true})` de @vueuse/core, un chemin qu'aucun test unitaire ne couvre.
  await page.getByPlaceholder('filter').fill('northern')
  await expect(page.locator('[data-preset]')).toHaveCount(1)
})

test('ajout et enregistrement d’une station, relus depuis l’API', async ({ page, request }) => {
  await page.goto('/plugins/radio/')
  await page.locator('[data-add]').click()
  const lignes = page.locator('[data-station-name]')
  await lignes.last().fill('Test E2E')
  await page.locator('[data-station-url]').last().fill('http://exemple.test/flux.mp3')
  await page.locator('[data-save]').click()
  const data = await (await request.get('/plugins/radio/api/data')).json()
  expect(data.stations.map((s: { name: string }) => s.name)).toContain('Test E2E')
  // Numerotation par position : la station de depart (stations.toml du
  // harness) occupe la présélection 1, la station ajoutee prend donc la 2.
  expect(data.stations.find((s: { name: string }) => s.name === 'Test E2E').preset).toBe(2)
})

test('apprentissage de touche : la vue atteint un état défini', async ({ page }) => {
  await page.goto('/plugins/generic-input/')
  const premiere = page.locator('[data-action-row]').first()
  await premiere.locator('[data-learn]').click()
  // Deux issues sont legitimes selon que l'environnement expose ou non un
  // peripherique evdev lisible, et les deux sont des etats definis :
  //  - aucun peripherique  -> « No input device detected »
  //  - apprentissage lance -> « Press a key on the device… »
  // On assert sur cet ensemble ferme de messages (valeurs de
  // crates/ritornello-plugin-generic-input/src/locales/en.toml — l'anglais
  // embarque, puisque RITORNELLO_LOCALES n'est step defini par le harness),
  // et non sur « un text quelconque » : un test qui accepte n'importe quoi
  // ne prouve rien.
  await expect(
    page.getByText(/No input device detected|Press a key on the device/),
  ).toBeVisible()
})

// L'onglet Système : rendu et navigation seulement. AUCUNE action
// d'alimentation n'est confirmée ici — le harness lance un vrai cœur sur la
// machine de développement, où confirm « Éteindre » l'arrêterait et
// « Redémarrer Ritornello » tuerait le harness en cours de route. Le
// dialog et l'envoi sont couverts par les tests vitest, qui n'ont step de
// machine à perdre.
test('onglet Système : métriques et boutons présents', async ({ page }) => {
  await page.goto('/system')
  // Deux assertions et non une : `not.toHaveText` seul passerait aussi sur une
  // page blanche — un locator absent ne peut step valoir « — ». La première
  // exige que le champ existe, la seconde qu'il porte une vraie valeur.
  await expect(page.locator('[data-system-kernel]')).toBeVisible()
  await expect(page.locator('[data-system-kernel]')).not.toHaveText('—')
  await expect(page.locator('[data-system-memory]')).toBeVisible()
  await expect(page.locator('[data-system-disk]')).toBeVisible()
  await expect(page.locator('[data-power-poweroff]')).toBeVisible()
  await expect(page.locator('[data-power-restart]')).toBeVisible()
  // Les dernières erreurs vivent ici, plus sur la page Configuration : c'est la
  // page qu'on ouvre quand l'appareil se comporte mal.
  await expect(page.locator('[data-logs-card]')).toBeVisible()
  await page.goto('/config')
  await expect(page.locator('[data-logs-card]')).toHaveCount(0)
  // Le lien de navigation existe depuis la page d'accueil. Scope a la nav du
  // haut (visible sur ce viewport bureau) : depuis la tache 11, une seconde
  // nav — la barre basse du phone — porte le meme lien, cachee mais
  // toujours dans le DOM, ce qui rendrait le selecteur nu ambigu.
  await page.goto('/')
  await expect(page.locator('[data-nav-haut] a[href="/system"]')).toBeVisible()
})
/**
 * **Le CSS d'un plugin ne doit step defaire celui du shell.**
 *
 * Regression reelle : les deux passes Tailwind ecrivaient dans la meme couche
 * `utilities`, et la feuille du plugin — injectee apres, et laissee en place a
 * dessein — gagnait a specificite egale. Le `class="hidden"` du champ de
 * fichier d'`InputAdmin` ecrasait ainsi le `md:flex` de la barre de navigation,
 * qui disparaissait pour le reste de la session.
 *
 * Un journey et step un test unitaire : le defaut ne vit ni dans le balisage ni
 * dans les composants mais dans la cascade CSS de deux feuilles reellement
 * servies, ce que jsdom ne calcule step.
 */
test('le menu du haut survit a une visite de generic-input', async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('[data-nav-haut]')).toBeVisible()

  // Navigation SPA (un clic), et non un `goto` : c'est le cas reel. Un
  // rechargement complet jetterait la feuille de style injectee, donc
  // masquerait justement le defaut.
  await page.locator('[data-nav-haut] a', { hasText: 'generic-input' }).click()
  await expect(page.locator('[data-device-select]')).toBeVisible()
  await expect(page.locator('[data-nav-haut]')).toBeVisible()

  // Et le champ de fichier du plugin — celui dont le `.hidden` ecrasait le
  // shell — reste bien masque : la couche `greffon` ne doit step avoir desarme
  // le CSS du plugin sur son propre balisage.
  await expect(page.locator('[data-import]')).toBeHidden()

  await page.locator('[data-nav-haut] a', { hasText: 'Configuration' }).click()
  await expect(page.locator('[data-nav-haut]')).toBeVisible()
})

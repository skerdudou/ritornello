import { expect, test } from '@playwright/test'

// Lit une variable CSS **calculee** sur la racine du document : c'est la
// seule preuve qu'un moteur de thème agit reellement sur le rendu — un
// attribut ou une classe ne prouve que l'intention, pas l'effet.
const variable = (page: import('@playwright/test').Page, nom: string) =>
  page.evaluate(
    (n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim(),
    nom,
  )

test('navigation entre l’accueil, la config et les pages de plugin', async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('[data-preset-button="1"]')).toBeVisible()
  // Le harnais ne déclare qu'une seule station (stations.toml) : la grille
  // n'affiche que ce numéro reel, et pas de fleches de pagination puisque le
  // compte ne dépasse pas neuf.
  await expect(page.locator('[data-preset-button]')).toHaveCount(1)
  await expect(page.locator('[data-preset-prev]')).toHaveCount(0)
  await expect(page.locator('[data-preset-next]')).toHaveCount(0)
  await page.goto('/config')
  // `getByText('radio')` seul est ambigu : l'en-tete liste aussi les
  // plugins admin par leur nom (voir App.vue), donc « radio » y apparait en
  // plus de la cellule du tableau de statut — d'ou ce ciblage par role.
  await expect(page.getByRole('cell', { name: 'radio' })).toBeVisible()

  // Une sauvegarde annonce son issue par une notification. Signale a l'usage :
  // la feuille de style de vue-sonner n'etait importee nulle part, si bien que
  // le message se rendait **dans le flux du document** -- un texte nu en bas de
  // page, qu'il fallait faire defiler pour voir. Rien d'autre que ce parcours ne
  // peut l'attraper : jsdom ne calcule aucun style, et l'assertion unitaire
  // equivalente passerait a tort.
  await page.locator('[data-seek-change]').click()
  const notif = page.locator('[data-sonner-toast]').first()
  await expect(notif).toBeVisible()
  // `fixed` : c'est la preuve que la feuille est chargee. Sans elle, le
  // conteneur reste en `static` et le message tombe au fil du document.
  await expect(page.locator('[data-sonner-toaster]')).toHaveCSS('position', 'fixed')
  // Page de plugin : le module ESM est charge dynamiquement et resolu par
  // l'import map — c'est ce qu'aucun test unitaire ne peut verifier.
  await page.goto('/plugins/radio/')
  await expect(page.locator('[data-save]')).toBeVisible()
  await page.goto('/plugins/generic-input/')
  // Vingt-trois depuis l'ajout des deux touches de deplacement dans la piste.
  // Ce compte est le seul verrou qui compte les lignes **rendues** : les tests
  // unitaires verrouillent la liste `ACTIONS` en amont, mais aucun d'eux ne
  // monte la page reelle.
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

test('l’état du lecteur arrive en flux poussé dès la connexion', async ({ page }) => {
  await page.goto('/')
  // Seule preuve de bout en bout de la route SSE : un `EventSource` reel du
  // navigateur, servi par le vrai binaire Rust. Aucun test unitaire ne couvre
  // la chaine complete (axum -> canal watch -> EventSource), et la propriete
  // verifiee ici est precisement celle qui evite un onglet vide : l'etat
  // courant est emis **des la connexion**, sans attendre un changement.
  const premiere = await page.evaluate(
    () =>
      new Promise<string>((resolve, reject) => {
        const flux = new EventSource('/api/player')
        const minuteur = setTimeout(() => {
          flux.close()
          reject(new Error('aucune trame en 5 s'))
        }, 5000)
        flux.onmessage = (e) => {
          clearTimeout(minuteur)
          flux.close()
          resolve(e.data as string)
        }
      }),
  )
  // Le harnais ne declare aucun plugin `metadata` et ne lit aucun flux reel :
  // l'etat est donc vide de metadonnees, mais il porte deja source et volume —
  // c'est ce qui permet a l'IHM d'afficher l'etat du lecteur sans sondage.
  const etat = JSON.parse(premiere) as {
    source: string
    volume: number
    muted: boolean
    standby: boolean
    preset: number | null
    title: string | null
  }
  expect(etat.source).toBe('radio')
  expect(etat.volume).toBeGreaterThan(0)
  expect(etat.muted).toBe(false)
  expect(etat.standby).toBe(false)
  // Le reveil de la radio joue la preselection 1 (stations.toml du harnais) et
  // la declare : c'est la preuve de bout en bout que la touche active voyage
  // du plugin jusqu'a la SPA. FIP n'emet pas d'ICY, donc pas de titre.
  expect(etat.preset).toBe(1)
  expect(etat.title).toBeNull()
  // Et l'encart du lecteur les affiche.
  await expect(page.locator('[data-source]')).toHaveText('radio')
  await expect(page.locator('[data-volume]')).toHaveText(`${etat.volume} %`)
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

test('la popin liste les 42 thèmes et les filtre', async ({ page }) => {
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
  // harnais) occupe la présélection 1, la station ajoutee prend donc la 2.
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
  // embarque, puisque RITORNELLO_LOCALES n'est pas defini par le harnais),
  // et non sur « un texte quelconque » : un test qui accepte n'importe quoi
  // ne prouve rien.
  await expect(
    page.getByText(/No input device detected|Press a key on the device/),
  ).toBeVisible()
})

// L'onglet Système : rendu et navigation seulement. AUCUNE action
// d'alimentation n'est confirmée ici — le harnais lance un vrai cœur sur la
// machine de développement, où confirmer « Éteindre » l'arrêterait et
// « Redémarrer Ritornello » tuerait le harnais en cours de route. Le
// dialogue et l'envoi sont couverts par les tests vitest, qui n'ont pas de
// machine à perdre.
test('onglet Système : métriques et boutons présents', async ({ page }) => {
  await page.goto('/system')
  // Deux assertions et non une : `not.toHaveText` seul passerait aussi sur une
  // page blanche — un locator absent ne peut pas valoir « — ». La première
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
  // Le lien de navigation existe depuis la page d'accueil.
  await page.goto('/')
  await expect(page.locator('a[href="/system"]')).toBeVisible()
})
